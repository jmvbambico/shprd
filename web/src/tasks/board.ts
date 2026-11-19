// Reading the board: the web's copy of the Herdr mods' `lib/board.js` and
// `lib/tokens.js`.
//
// The service folds the ledger and derives each task's word itself, so the web
// never decides what a task is doing: it shows `derived.status`, `derived.need`
// and `derived.reason` as they arrive. These functions are the same rules the
// Tasks panel on the desktop follows, so a row, the CLI's `status` and the
// phone never disagree.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/tasks.js` (the row's second line), `mods/tasks/main.js`
//   (`STATUS`, the grouping order, the filter row, the search)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { AttemptDto, DerivedStatusDto, NeedKindDto, ReviewDto, SnapshotDto, TaskDto } from "../gen";

/** The tone a status is drawn in. */
export type Tone = "dim" | "amber" | "red" | "green";

/** The shape of the glyph that sits beside a task's word. */
export type Glyph = "ring" | "calendar" | "clock" | "pie" | "dot" | "tick" | "spin";

/** The eight words, each with its tone and glyph. */
export const STATUS: Record<DerivedStatusDto, { word: string; tone: Tone; glyph: Glyph }> = {
  to_do: { word: "To do", tone: "dim", glyph: "ring" },
  scheduled: { word: "Scheduled", tone: "dim", glyph: "calendar" },
  queued: { word: "Queued", tone: "dim", glyph: "clock" },
  working: { word: "Working", tone: "amber", glyph: "pie" },
  needs_you: { word: "Needs you", tone: "red", glyph: "dot" },
  ready: { word: "Ready", tone: "green", glyph: "tick" },
  merging: { word: "Merging", tone: "green", glyph: "spin" },
  done: { word: "Done", tone: "dim", glyph: "tick" },
};

/** The eight, in the order a list groups them. */
export const STATUS_ORDER: DerivedStatusDto[] = [
  "needs_you",
  "ready",
  "working",
  "merging",
  "queued",
  "scheduled",
  "to_do",
  "done",
];

/** Why a task reads *Needs you*, by the API's `derived.need`. */
export const NEED: Record<NeedKindDto, string> = {
  attention: "Only you can settle this",
  asks: "Asks you something",
  reply: "Waits for your reply",
  stopped: "Agent stopped",
  provider: "Its provider cut it off",
};

/** A task reference as a person reads it, capped so a row keeps its second line. */
export function ref(id: string | null | undefined, max = 14): string {
  const text = String(id ?? "");
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** One status, or `to_do` for a word this build does not know. */
export function look(task: TaskDto | null | undefined): { word: string; tone: Tone; glyph: Glyph } {
  const word = task?.derived?.status;
  return (word !== undefined && STATUS[word]) || STATUS.to_do;
}

/** How many characters a row's second line may hold. */
export const LINE_MAX = 40;

/** How many characters the engine's "Waiting: …" line may hold. */
export const WAITING_MAX = 120;

/** Cuts `text` to `max` characters, ending in an ellipsis when it was cut. */
export function cut(text: unknown, max = LINE_MAX): string {
  const value = String(text ?? "").trim();
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}

/**
 * A task's second line: the card's own reason, then why it needs the person,
 * then the agent's last progress line, then its title.
 */
export function secondLine(task: TaskDto | null | undefined): string {
  const derived = task?.derived;
  const said = derived?.reason ?? (derived?.need ? NEED[derived.need] : null);
  if (said) {
    return cut(said);
  }
  // A task that handed in is past its agent's progress line: that line is the
  // last thing the worker said while working, and stale once a reviewer runs.
  const handed = handedInLine(task);
  if (handed) {
    return handed;
  }
  const line = task?.last_status?.message;
  if (line) {
    // The engine's own "Waiting: <why>" line keeps the room it was written with.
    return cut(line, line.startsWith("Waiting: ") ? WAITING_MAX : LINE_MAX);
  }
  return cut(task?.title ?? "");
}

/**
 * What a task that handed its work in is waiting on, in a word: its review, its
 * gates or the person. Null for a task still being worked on.
 */
export function handedInLine(task: TaskDto | null | undefined): string | null {
  if (task?.status === "passed") {
    return "Reviewed";
  }
  if (task?.status !== "reported") {
    return null;
  }
  const latest = (task.attempts ?? [])[task.attempts.length - 1];
  const reviewed = (latest?.gates ?? []).some((gate) => gate.gate === "review");
  return (latest?.reviews ?? []).length > 0 && !reviewed ? "In review" : "Running its gates";
}

/** The percent an agent reported while it worked; null once it handed in, so a stale one is not drawn. */
export function progressOf(task: TaskDto | null | undefined): number | undefined {
  return handedInLine(task) === null ? task?.last_status?.progress : undefined;
}

/** Whether a task still wants drawing: a dropped task is history. */
export function isLive(task: TaskDto | null | undefined): boolean {
  return task?.status !== "dropped";
}

/** The counts a badge shows, over the live tasks. */
export function counts(state: SnapshotDto | null): { needs: number; ready: number; total: number } {
  const tally = { needs: 0, ready: 0, total: 0 };
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) {
      continue;
    }
    tally.total += 1;
    if (task.derived.status === "needs_you") {
      tally.needs += 1;
    } else if (task.derived.status === "ready") {
      tally.ready += 1;
    }
  }
  return tally;
}

/** Done rows a group shows before "Show N more". */
export const DONE_SHOWN = 5;

/** The words a filter chip may name besides "all". */
export const FILTER_PRIMARY: DerivedStatusDto[] = ["needs_you", "working", "ready"];

/** Whether every word of `query` is somewhere in one of `fields` (case-blind, any order). */
export function matches(query: string, ...fields: unknown[]): boolean {
  const words = String(query ?? "")
    .toLowerCase()
    .split(/\s+/)
    .filter((w) => w !== "");
  if (words.length === 0) {
    return true;
  }
  const hay = fields
    .filter((f) => f !== null && f !== undefined)
    .map((f) => String(f).toLowerCase())
    .join("\n");
  return words.every((w) => hay.includes(w));
}

/** Whether the search finds `task`: id, title, second line, status word, PR number. */
export function found(task: TaskDto, query: string): boolean {
  const word = STATUS[task.derived.status]?.word ?? null;
  const pr = task.pr?.number ? `#${task.pr.number}` : null;
  return matches(query, task.id, task.title, secondLine(task), word, pr);
}

/** One chip of the filter row. */
export interface Chip {
  key: DerivedStatusDto;
  word: string;
  count: number;
}

/** The live tasks per word, in list order, leaving out words nobody is in. */
export function statusCounts(state: SnapshotDto | null): Chip[] {
  const by = new Map<DerivedStatusDto, number>(STATUS_ORDER.map((w) => [w, 0]));
  for (const task of state?.tasks ?? []) {
    if (isLive(task) && by.has(task.derived.status)) {
      by.set(task.derived.status, (by.get(task.derived.status) ?? 0) + 1);
    }
  }
  return STATUS_ORDER.filter((w) => (by.get(w) ?? 0) > 0).map((w) => ({
    key: w,
    word: STATUS[w].word,
    count: by.get(w) ?? 0,
  }));
}

/** The chips drawn (Needs you, Working, Ready, when they have rows) and the ones under "More". */
export function filterChips(state: SnapshotDto | null): { primary: Chip[]; more: Chip[] } {
  const rows = statusCounts(state);
  return {
    primary: FILTER_PRIMARY.map((k) => rows.find((r) => r.key === k)).filter(
      (r): r is Chip => r !== undefined,
    ),
    more: rows.filter((r) => !FILTER_PRIMARY.includes(r.key)),
  };
}

/** One folding group of the list. */
export interface Group {
  key: string;
  label: string;
  top: boolean;
  tasks: TaskDto[];
  count: number;
  shut: boolean;
  cut: number;
  more: boolean;
  flat?: boolean;
}

/** What the list takes besides the board. */
export interface ListOptions {
  query?: string;
  filter?: string;
  shut?: ReadonlySet<string>;
  doneOpen?: boolean;
  allDone?: boolean;
}

/**
 * What the Tasks list draws.
 *
 * - `all`: "For you" first (needs you, then Ready), then each other word in
 *   order; Done is one line ("N done") until `doneOpen`.
 * - a status word: that word's tasks, flat, under no heading.
 * - a query: every finding, Done included, none folded.
 */
export function listView(
  state: SnapshotDto | null,
  { query = "", filter = "all", shut = new Set<string>(), doneOpen = false, allDone = false }: ListOptions = {},
): { groups: Group[]; doneLine: { count: number; label: string } | null } {
  const searching = query.trim() !== "";
  const live = (state?.tasks ?? []).filter(isLive).filter((t) => !searching || found(t, query));
  const word = (t: TaskDto) => t.derived.status;
  const make = (key: string, label: string, tasks: TaskDto[], flat = false): Group => {
    const done = key === "done";
    const all = searching || allDone;
    const hidden = done && !all && tasks.length > DONE_SHOWN ? tasks.length - DONE_SHOWN : 0;
    return {
      key,
      label,
      top: key === "you",
      count: tasks.length,
      shut: !searching && shut.has(key),
      cut: hidden,
      more: done && tasks.length > DONE_SHOWN,
      tasks: hidden > 0 ? tasks.slice(0, tasks.length - hidden) : tasks,
      ...(flat ? { flat: true } : {}),
    };
  };
  if (filter !== "all" && filter in STATUS) {
    const tasks = live.filter((t) => word(t) === filter);
    return {
      groups: tasks.length === 0 ? [] : [make(`flat-${filter}`, STATUS[filter as DerivedStatusDto].word, tasks, true)],
      doneLine: null,
    };
  }
  const groups: Group[] = [];
  const you = [...live.filter((t) => word(t) === "needs_you"), ...live.filter((t) => word(t) === "ready")];
  if (you.length > 0) {
    groups.push(make("you", "For you", you));
  }
  let doneLine: { count: number; label: string } | null = null;
  for (const w of STATUS_ORDER) {
    if (w === "needs_you" || w === "ready") {
      continue;
    }
    const tasks = live.filter((t) => word(t) === w);
    if (tasks.length === 0) {
      continue;
    }
    if (w === "done" && !searching && !doneOpen) {
      doneLine = { count: tasks.length, label: `${tasks.length} done` };
      continue;
    }
    groups.push(make(w, STATUS[w].word, tasks));
  }
  return { groups, doneLine };
}

/** The task a selection names, from the board it is read from (null once it left). */
export function taskById(state: SnapshotDto | null, id: string | null | undefined): TaskDto | null {
  if (!id) {
    return null;
  }
  return (state?.tasks ?? []).find((task) => task.id === id) ?? null;
}

/** The newest attempt of a task, when it has one. */
export function attempt(task: TaskDto | null | undefined): AttemptDto | null {
  const attempts = task?.attempts ?? [];
  return attempts.length === 0 ? null : (attempts[attempts.length - 1] ?? null);
}

/** A task's latest review, when one was dispatched. */
export function review(task: TaskDto | null | undefined): ReviewDto | null {
  const reviews = attempt(task)?.reviews ?? [];
  return reviews.length === 0 ? null : (reviews[reviews.length - 1] ?? null);
}

/** The one act a row offers. `action` is null for Review, which opens the Accept card. */
export interface RowAct {
  id: "review" | "start" | "retry" | "stop" | "drop";
  label: string;
  action: "task.start" | "task.retry" | "task.stop" | null;
}

/**
 * The one act a row offers — or nothing. A verb is offered only where the
 * engine's rules would accept it.
 */
export function rowActions(task: TaskDto | null | undefined): RowAct[] {
  if (!task || !isLive(task)) {
    return [];
  }
  const word = task.derived.status;
  const status = task.status;
  if (word === "ready" || status === "passed" || status === "reported") {
    return [{ id: "review", label: "Review", action: null }];
  }
  // A task that is stuck (stopped, failed, waiting on the person) can be given
  // up from its row. Drop needs a reason, so it opens the card's Drop step.
  const drop: RowAct[] =
    status === "failed" || word === "needs_you" ? [{ id: "drop", label: "Drop", action: null }] : [];
  if (status === "queued" && (word === "to_do" || word === "scheduled")) {
    return [{ id: "start", label: "Start", action: "task.start" }];
  }
  if (status === "failed") {
    return [{ id: "retry", label: "Retry", action: "task.retry" }, ...drop];
  }
  if (status === "working" && word === "working") {
    return [{ id: "stop", label: "Stop", action: "task.stop" }];
  }
  return drop;
}

/** What the add-task form says about its fields. */
export interface NewTaskCheck {
  title: string;
  scope: string[];
  acceptance: string[];
  deps: string[];
  why: string;
  ready: boolean;
}

/**
 * The add-task form's reading of its fields: a title, the paths it may change
 * (comma separated) and what must be true when it is done (semicolon separated).
 */
export function newTask(
  body: { title?: string; scope?: string; acceptance?: string; deps?: string },
  known: readonly string[] | null = null,
): NewTaskCheck {
  const title = String(body.title ?? "").trim();
  const split = (text: string | undefined, sep: string) =>
    String(text ?? "")
      .split(sep)
      .map((part) => part.trim())
      .filter((part) => part !== "");
  const scope = split(body.scope, ",");
  const acceptance = split(body.acceptance, ";");
  const deps = [...new Set(String(body.deps ?? "").split(/[\s,]+/).filter((id) => id !== ""))];
  const unknown = known === null ? [] : deps.filter((id) => !known.includes(id));
  const why =
    title === ""
      ? "A task needs a title"
      : scope.length === 0
        ? "Name the folder or path it may change"
        : acceptance.length === 0
          ? "Say what must be true when it is done"
          : unknown.length > 0
            ? `No task named ${unknown.join(", ")} to wait for`
            : "";
  return { title, scope, acceptance, deps, why, ready: why === "" };
}

/** How long ago `at` was: `just now`, `4m ago`, `2h ago`. */
export function since(at: number, now = Date.now()): string {
  const ms = Number.isFinite(at) ? now - at : 0;
  if (ms < 45_000) {
    return "just now";
  }
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h ago`;
  }
  return `${Math.floor(hours / 24)}d ago`;
}
