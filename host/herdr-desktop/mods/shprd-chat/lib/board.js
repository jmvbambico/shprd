// Reading the board: the one place a mod turns `GET /api/state` into the lines a
// panel draws.
//
// The service folds the ledger and derives every task's word itself
// (`crates/shprd-server/src/snapshot.rs`, `crates/shprd-views/src/derived.rs`), so
// a mod never decides what a task is doing: it shows `derived.status`,
// `derived.need` and `derived.reason` as they arrive. That is the rule
// Shepherdr's window follows too (its `core/tasks.js` derives status from live
// terminals, and the window only renders it), and it is what keeps a card, the
// CLI's `shprd status` and this panel from disagreeing.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/tasks.js` (the row's second line and the reference a person
//   reads), `mods/tasks/main.js` (`STATUS`, the grouping order)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/board.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/board.js`. Edit the source, never a copy.

import { NEED, STATUS, STATUS_ORDER, status as statusOf } from "./tokens.js";

/** How many characters a row's second line may hold (Shepherdr `LINE_MAX`). */
export const LINE_MAX = 40;

/** How many characters the engine's "Waiting: …" line may hold. */
export const WAITING_MAX = 120;

/** Cuts `text` to `max` characters, ending in an ellipsis when it was cut. */
export function cut(text, max = LINE_MAX) {
  const value = String(text ?? "").trim();
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}

/**
 * A task's second line: what it says now.
 *
 * The order is Shepherdr's (`mods/tasks/main.js`, the row's `says`): the card's
 * own reason first, then why it needs the person, then the agent's last progress
 * line, then its title. A row that says nothing at all is not drawn, so this
 * always answers something.
 */
export function secondLine(task) {
  const derived = task?.derived ?? {};
  // A queued task the engine holds back because another task has the same file says so
  // in plain words ("waits for T3: both change tcalc/ops.py"), not as a path in use.
  const held = heldBy(task);
  if (held && !task?.pending?.label) return cut(`waits for ${held.task}: both change ${held.path}`, WAITING_MAX);
  // A task whose action the service has not answered yet says what it is doing
  // ("Merging…", "Stopping…"): the optimistic layer's label (SHPR-81).
  if (task?.pending?.label) return task.pending.label;
  // A done task says it is done: its report's summary when it has one, else the
  // word. Never what its agent last said or the card's old reason ("No longer
  // waiting", "Handing in report"), which was true only while it worked.
  if (derived.status === "done") return doneLine(task);
  const said = derived.reason ?? (derived.need ? NEED[derived.need] : null);
  if (said) return cut(said);
  // A task that handed in is past its agent's progress line: that line is the
  // last thing the worker said while working, and stale once a reviewer runs.
  const handed = handedInLine(task);
  if (handed) return handed;
  const line = task?.last_status?.message;
  // The engine's own "Waiting: <why>" line says why a step is refused: it keeps
  // the room it was written with (120) so the path and task id are not cut.
  if (line) return cut(line, line.startsWith("Waiting: ") ? WAITING_MAX : LINE_MAX);
  return cut(task?.title ?? "");
}

/**
 * The task (and the file) that holds a queued task back, from the engine's own line
 * "Waiting: <path> is in use by <task>", or null. The tasks have no dependency on each
 * other; they only change the same file, so the later one starts when the first lands.
 */
export function heldBy(task) {
  const word = task?.derived?.status;
  if (task?.status !== "queued" || (word !== "to_do" && word !== "queued" && word !== "scheduled")) return null;
  const m = /^Waiting: (.+) is in use by (\S+)$/.exec(String(task?.last_status?.message ?? "").trim());
  return m ? { path: m[1], task: m[2] } : null;
}

/** The second line of a done task: its report's summary, else "Done". */
export function doneLine(task) {
  const reports = (task?.attempts ?? []).map((a) => a?.report?.summary).filter((x) => String(x ?? "").trim() !== "");
  const summary = reports.length > 0 ? String(reports[reports.length - 1]).replace(/\s+/g, " ") : "";
  return summary !== "" ? cut(summary) : "Done";
}

/**
 * What a task that handed its work in is waiting on, in a word: its review, its
 * gates or the person. Null for a task still being worked on.
 */
export function handedInLine(task) {
  if (task?.status === "passed") return "Reviewed";
  if (task?.status !== "reported") return null;
  const attempts = task.attempts ?? [];
  const latest = attempts[attempts.length - 1];
  const reviewed = (latest?.gates ?? []).some((gate) => gate.gate === "review");
  return (latest?.reviews ?? []).length > 0 && !reviewed ? "In review" : "Running its gates";
}

/** Whether a task still wants drawing: a dropped task is history, not work. */
export function isLive(task) {
  return task?.status !== "dropped";
}

/** The counts a badge or the status bar shows, over the live tasks. */
export function counts(state, now = Date.now()) {
  const tally = { needs: 0, ready: 0, working: 0, done: 0, total: 0, open_asks: 0 };
  const old = staleIds(state, now);
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    tally.total += 1;
    const word = task?.derived?.status;
    // An old leftover does not make the badge shout: it sits under "Older".
    if (word === "needs_you") tally.needs += old.has(task.id) ? 0 : 1;
    else if (word === "ready") tally.ready += 1;
    else if (word === "working" || word === "merging") tally.working += 1;
    else if (word === "done") tally.done += 1;
  }
  tally.open_asks = (state?.asks ?? []).length;
  return tally;
}

/**
 * Every live task, grouped the way the panel draws them: the eight words in
 * [`STATUS_ORDER`], each group only when it has rows.
 *
 * `grouped` is a list of `{status, word, tasks}`; an empty list is a board with
 * nothing on it, which is a run nobody started rather than a failure.
 */
export function grouped(state) {
  const byWord = new Map(STATUS_ORDER.map((word) => [word, []]));
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    const word = task?.derived?.status;
    if (byWord.has(word)) byWord.get(word).push(task);
  }
  const groups = [];
  for (const word of STATUS_ORDER) {
    const tasks = byWord.get(word) ?? [];
    if (tasks.length === 0) continue;
    groups.push({ status: word, word: STATUS[word].word, tasks });
  }
  return groups;
}

/** How long an open task may sit untouched before the board calls it old (3 hours). */
export const STALE_MS = 3 * 60 * 60 * 1000;

/** The words of a task that is open and waiting: it needs the person, or has not started. */
const WAITING_WORDS = ["needs_you", "to_do", "scheduled", "queued"];

/**
 * The ids of the tasks that are old leftovers: open (Needs you, or Queued and not
 * started) and untouched for [`STALE_MS`], plus a Queued task that waits on one of
 * them (it can never start while its dependency is stuck).
 *
 * A task the service gave no `touched_at_ms` (an older service) is never old, and
 * Working, Ready, Merging and Done are never old: they are moving, or waiting for
 * a decision the person is expected to make on fresh work.
 */
export function staleIds(state, now = Date.now()) {
  const tasks = (state?.tasks ?? []).filter(isLive);
  const stale = new Set();
  for (const task of tasks) {
    const touched = task?.touched_at_ms;
    if (!Number.isFinite(touched) || touched <= 0) continue;
    if (!WAITING_WORDS.includes(task?.derived?.status)) continue;
    if (now - touched >= STALE_MS) stale.add(task.id);
  }
  // A queued task behind an old one is old with it, however recently it was added.
  let grew = true;
  while (grew) {
    grew = false;
    for (const task of tasks) {
      if (stale.has(task.id)) continue;
      if (!["to_do", "scheduled", "queued"].includes(task?.derived?.status)) continue;
      if ((task.deps ?? []).some((dep) => stale.has(dep))) {
        stale.add(task.id);
        grew = true;
      }
    }
  }
  return stale;
}

/** Newest first: a task added later sorts before one added earlier. */
export function newestFirst(a, b) {
  return (b?.added_at_ms ?? 0) - (a?.added_at_ms ?? 0);
}

/** Done rows a group shows before "Show N more" (Shepherdr `DONE_SHOWN`). */
export const DONE_SHOWN = 5;

/** The words a filter chip may name, besides "all". */
export const FILTER_PRIMARY = ["needs_you", "working", "ready"];

/**
 * Whether every word of `query` is somewhere in one of `fields`.
 *
 * Shepherdr's `matches` (`mods/tasks/main.js`): case-blind, words in any order,
 * an empty query finds everything.
 */
export function matches(query, ...fields) {
  const words = String(query ?? "").toLowerCase().split(/\s+/).filter((w) => w !== "");
  if (words.length === 0) return true;
  const hay = fields
    .filter((f) => f !== null && f !== undefined)
    .map((f) => String(f).toLowerCase())
    .join("\n");
  return words.every((w) => hay.includes(w));
}

/** Whether the search finds `task`: id, title, second line, status word, PR number. */
export function found(task, query) {
  const word = STATUS[task?.derived?.status]?.word ?? null;
  const pr = task?.pr?.number ? `#${task.pr.number}` : null;
  return matches(query, task?.id, task?.title, secondLine(task), word, pr);
}

/**
 * Minutes left when the service reports an estimate, else null.
 *
 * The service does not send one today; a task that carries `eta_minutes` (or
 * `derived.eta_minutes`) shows it under its glyph, and one that does not shows
 * nothing rather than a guess.
 */
export function etaMinutes(task) {
  const raw = task?.eta_minutes ?? task?.derived?.eta_minutes;
  return Number.isFinite(raw) && raw > 0 ? Math.ceil(raw) : null;
}

/**
 * The filter row's chips over the live tasks: `{key, word, count}` for each word
 * a task is in, split into the chips drawn (Needs you, Working, Ready, when they
 * have rows) and the ones that go under "More".
 */
export function filterChips(state) {
  const rows = statusCounts(state);
  const chip = (r) => ({ key: r.status, word: r.word, count: r.count });
  return {
    primary: FILTER_PRIMARY.map((k) => rows.find((r) => r.status === k)).filter(Boolean).map(chip),
    more: rows.filter((r) => !FILTER_PRIMARY.includes(r.status)).map(chip),
  };
}

/** The ways the Tasks list can be grouped (the header's Group by menu). */
export const GROUP_MODES = [
  { key: "you", label: "For you" },
  { key: "space", label: "Space" },
  { key: "status", label: "Status" },
];

/** A remembered group-by word, or `you` for anything this build does not know. */
export function groupMode(value) {
  return GROUP_MODES.some((m) => m.key === value) ? value : "you";
}

/**
 * What the Tasks list draws, as `{groups, doneLine}`.
 *
 * - `all` (default): grouped by `groupBy`:
 *   - `you` (default): "For you" first (needs you, then Ready), then each other
 *     word in [`STATUS_ORDER`]; Done is one line ("N done") until `doneOpen`,
 *     then a group showing [`DONE_SHOWN`] rows and "Show N more" unless `allDone`.
 *   - `status`: one group per word, needs you and Ready included, same Done rule.
 *   - `space`: one group per space in `spaces` (`{id, name}`), tasks in status
 *     order, Done rows included.
 *   Old leftovers (see [`staleIds`]) leave every grouping for one folded "Older"
 *   group at the bottom (`stale: true`, `ids`: what a "Clear old tasks" drops),
 *   open with `olderOpen`; inside a word the newest task is on top.
 * - a status word: that word's tasks, flat, under no heading.
 * - a query: every finding, Done included, all shown, headings not folding.
 *
 * `shut` is the set of folded heading keys. A group is
 * `{key, label, top, tasks, count, shut, cut, more}`.
 */
export function listView(
  state,
  {
    query = "",
    filter = "all",
    shut = new Set(),
    doneOpen = false,
    allDone = false,
    groupBy = "you",
    spaces = [],
    now = Date.now(),
    olderOpen = false,
  } = {},
) {
  const searching = String(query ?? "").trim() !== "";
  const live = (state?.tasks ?? []).filter(isLive).filter((t) => !searching || found(t, query));
  const word = (t) => t?.derived?.status;
  const rank = (t) => {
    const at = STATUS_ORDER.indexOf(word(t));
    return at === -1 ? STATUS_ORDER.length : at;
  };
  // Newest first inside a word, so what the coordinator just added is on top.
  const byRank = (list) => [...list].sort((a, b) => rank(a) - rank(b) || newestFirst(a, b));
  const make = (key, label, tasks, extra = {}) => {
    const done = key === "done";
    const open = searching;
    const all = open || allDone;
    const cut = done && !all && tasks.length > DONE_SHOWN ? tasks.length - DONE_SHOWN : 0;
    return {
      key,
      label,
      top: key === "you",
      count: tasks.length,
      shut: !open && shut.has(key),
      cut,
      more: done && tasks.length > DONE_SHOWN,
      tasks: cut > 0 ? tasks.slice(0, tasks.length - cut) : tasks,
      ...extra,
    };
  };
  if (filter !== "all" && STATUS[filter]) {
    const tasks = live.filter((t) => word(t) === filter);
    return { groups: tasks.length === 0 ? [] : [make(`flat-${filter}`, STATUS[filter].word, tasks, { flat: true })], doneLine: null };
  }
  const groups = [];
  // Old leftovers (stale open tasks and what waits on them) leave the groups and
  // sit in one folded "Older" group at the bottom, which a heading button clears.
  // A search still finds them in place.
  const old = searching ? new Set() : staleIds(state, now);
  const rest = live.filter((t) => !old.has(t.id));
  const older = byRank(live.filter((t) => old.has(t.id)));
  const olderGroup = () =>
    older.length === 0
      ? []
      : [
          make("older", "Older", older, {
            shut: !olderOpen,
            stale: true,
            ids: older.map((t) => t.id),
          }),
        ];
  let doneLine = null;
  if (groupBy === "space") {
    const list = spaces.length > 0 ? spaces : [{ id: null, name: "Tasks" }];
    const home = list[0].id;
    const known = new Set(list.map((space) => space.id));
    for (const space of list) {
      let tasks = byRank(
        rest.filter((t) => (t.space != null && known.has(t.space) ? t.space : home) === space.id),
      );
      // Done is one folded line, then one Done group, like the other groupings:
      // finished work does not fill a space's rows (a search still finds it there).
      if (!searching) tasks = tasks.filter((t) => word(t) !== "done");
      if (tasks.length > 0) groups.push(make(`space-${space.id}`, space.name ?? String(space.id), tasks));
    }
    const finished = [...rest.filter((t) => word(t) === "done")].sort(newestFirst);
    if (!searching && finished.length > 0) {
      if (doneOpen) groups.push(make("done", STATUS.done.word, finished));
      else doneLine = { count: finished.length, label: `${finished.length} done` };
    }
    groups.push(...olderGroup());
    return { groups, doneLine };
  }
  const first = groupBy === "status" ? [] : ["needs_you", "ready"];
  if (groupBy !== "status") {
    const you = byRank([...rest.filter((t) => word(t) === "needs_you"), ...rest.filter((t) => word(t) === "ready")]);
    if (you.length > 0) groups.push(make("you", "For you", you));
  }
  for (const w of STATUS_ORDER) {
    if (first.includes(w)) continue;
    const tasks = [...rest.filter((t) => word(t) === w)].sort(newestFirst);
    if (tasks.length === 0) continue;
    if (w === "done" && !searching && !doneOpen) {
      doneLine = { count: tasks.length, label: `${tasks.length} done` };
      continue;
    }
    groups.push(make(w, STATUS[w].word, tasks));
  }
  groups.push(...olderGroup());
  return { groups, doneLine };
}

/**
 * The task a panel's shared selection names, from the board it is reading.
 *
 * The board is the only place a task is looked up: a panel shares *which* task it
 * is showing (`shared("tasks.thread")`, Shepherdr's own contract) and never a copy
 * of the task, so two views of one drill-in cannot drift apart. A task that is no
 * longer on the board answers null — it landed or was dropped while a view was
 * open, and that is a calm answer, not a missing one.
 */
export function taskById(state, id) {
  if (!id) return null;
  return (state?.tasks ?? []).find((task) => task.id === id) ?? null;
}

/** One task's status, as the tone and glyph a row draws. */
export function look(task) {
  return { ...statusOf(task?.derived?.status), pct: progressOf(task) };
}

/**
 * The percent an agent reported (`last_status.progress`), which fills a working
 * task's pie; null when it reported none, and the pie reads half full.
 */
export function progressOf(task) {
  if (handedInLine(task) !== null) return null;
  const done = task?.last_status?.progress;
  return typeof done === "number" && Number.isFinite(done) && done >= 0 ? done : null;
}

/**
 * The live tasks per word, in the order a panel groups them: what needs the
 * person first, then what moves, then what waits, then what is over.
 *
 * `counts` answers the four numbers a badge shows; this answers all eight, for a
 * status bar that says how the whole board stands. A word nobody's task is in
 * is left out, so a bar never draws a row of zeroes.
 */
export function statusCounts(state) {
  const byWord = new Map(STATUS_ORDER.map((word) => [word, 0]));
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    const word = task?.derived?.status;
    if (byWord.has(word)) byWord.set(word, byWord.get(word) + 1);
  }
  const rows = [];
  for (const word of STATUS_ORDER) {
    const count = byWord.get(word) ?? 0;
    if (count === 0) continue;
    rows.push({ status: word, word: STATUS[word].word, tone: STATUS[word].tone, count });
  }
  return rows;
}

/**
 * The run's own state as one word: what the status bar says at the left.
 *
 * A service that answered but has no run is not the same as a service that is
 * not there — the bar draws `down` for the second and this for the first.
 */
export function runState(state) {
  if (!state?.run) return "none";
  return state.finished ? "finished" : "running";
}

/** How long ago `at` was, in words a bar can show (`just now`, `4m`, `2h`). */
export function since(at, now = Date.now()) {
  const ms = Number.isFinite(at) ? now - at : 0;
  if (ms < 45_000) return "just now";
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/**
 * What the window says when the repository has no run yet: where to go next, in
 * words that name no command. The window never reads the service's own `hint`
 * for it (an older service's says `shprd run start`): nobody types a command in
 * the shprd layout, a run starts with the first task the chat makes.
 */
export const NO_TASKS = "no tasks yet: say what you want in the chat";

/**
 * The run as the status bar names it: the run's id, else [`NO_TASKS`], so a bar
 * with no run is never a dead end (the M9 gate's bug 6 was that silence).
 */
export function runLabel(state) {
  const run = state?.run;
  if (!run) return NO_TASKS;
  return `run ${run.id} · ${run.integration_branch}`;
}

/**
 * The run as a *status bar* names it: its id alone, because the branch it lands
 * on is on the sidebar's coordinator row already and the bar has one line for
 * everything.
 *
 * A bar with no run says [`NO_TASKS`], exactly as [`runLabel`] does.
 */
export function runShort(state) {
  const run = state?.run;
  if (!run) return NO_TASKS;
  // The run's name and then where its tasks stand now, so the bar follows the
  // board instead of naming a run whose work is long finished (SHPR-79).
  const tally = counts(state);
  const parts = [];
  if (tally.working > 0) parts.push(`${tally.working} working`);
  if (tally.ready > 0) parts.push(`${tally.ready} ready`);
  if (parts.length === 0 && tally.total > 0 && tally.done === tally.total) parts.push("all done");
  return [`run ${run.id}`, ...parts].join(" · ");
}

/** The newest attempt of a task, when it has one. */
export function attempt(task) {
  const attempts = task?.attempts ?? [];
  return attempts.length === 0 ? null : attempts[attempts.length - 1];
}

/** A task's latest review, when one was dispatched. */
export function review(task) {
  const latest = attempt(task);
  const reviews = latest?.reviews ?? [];
  return reviews.length === 0 ? null : reviews[reviews.length - 1];
}

/**
 * The last pass's first failure, one line, or null when the last pass was clean.
 *
 * The service keeps it (`last_tick`) so a step that failed is visible on the
 * board as well as in the service's own output; a panel that hid it would leave
 * a run that looks idle when it is stuck.
 */
export function lastFailure(state) {
  const failed = state?.last_tick?.failed ?? [];
  if (failed.length === 0) return null;
  const first = failed[0];
  const where = first.task ? ` on ${first.task}` : "";
  const others = failed.length - 1;
  const text =
    first.step === "land" && /^Couldn't /.test(first.error ?? "")
      ? cut(first.error, 80)
      : `${first.step}${where}: ${cut(first.error, 80)}`;
  return `${text}${others > 0 ? ` (+${others})` : ""}`;
}

/**
 * The one act a row offers, as `{id, label, action}` — or nothing.
 *
 * The verbs are the engine's, and a row offers one only where that verb would
 * be accepted: a start needs a task the rules call queued, a retry needs one
 * whose attempt failed (a send-back is a spawn, and a spawn is refused while any
 * attempt is unfinished), and a stop needs an agent that is supposed to be
 * running. A ready row's act is not an action at all: it opens the Accept card,
 * so `action` is null and the row knows to drill in.
 *
 * Nothing is offered for a task that is merging, done or dropped: those are the
 * engine's or history's, and a button that always refuses is worse than none.
 */
export function rowActions(task) {
  const word = task?.derived?.status;
  const status = task?.status;
  if (!isLive(task)) return [];
  // A task that is stuck (stopped, failed, waiting on the person) can be given up
  // from its row. Drop needs a reason, so it opens the card on its Drop step.
  const drop = status === "failed" || word === "needs_you" ? [{ id: "drop", label: "Drop", action: null }] : [];
  // The button follows the word the row shows (SHPR-78): Review only where the row
  // reads Ready, or needs the person about a hand-in. A task that handed in and
  // whose agent took the work up again reads Working (or Merging): its status is
  // still `passed`/`reported`, but there is nothing to review, and a Stop the
  // engine would refuse is worse than no button, so such a row has none.
  const handedIn = status === "passed" || status === "reported";
  if (word === "ready" || (handedIn && (word === "needs_you" || word == null))) {
    return [{ id: "review", label: "Review", action: null }, ...(word === "needs_you" ? drop : [])];
  }
  if (handedIn) return [];
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

/**
 * What a per-repository list shows before and while its board is read (SHPR-78).
 *
 * - a cached board: `list` (drawn at once, refreshed in place);
 * - none cached, and the repository is known to hold tasks (`count > 0`):
 *   `loading` ("Loading tasks…");
 * - none cached, and it is known to be empty (`count === 0`): `empty` at once,
 *   never a loading line for a repository with nothing to load;
 * - none cached and the count not back yet: `neutral`, a blank body for a moment,
 *   never "Nothing here".
 *
 * `count` is null/undefined while unknown.
 */
export function paneState({ cached = false, count = null } = {}) {
  if (cached) return "list";
  if (!Number.isFinite(count)) return "neutral";
  return count > 0 ? "loading" : "empty";
}

/**
 * The tasks a panel can offer to start, and the ones it must leave alone.
 *
 * It is the add-task form's reading of the board: what the service would accept
 * as a scope (`a path, or a folder`) is not decided here — the rules are — but a
 * title and one acceptance line are what the form asks for, and a form that sent
 * an empty one would be refused for it. `id` also says why a form cannot be
 * submitted yet, in words, or is empty when it can.
 */
export function newTask(body, known = null) {
  const title = String(body?.title ?? "").trim();
  const scope = String(body?.scope ?? "")
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const acceptance = String(body?.acceptance ?? "")
    .split(";")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const deps = splitIds(body?.deps);
  const unknown = known === null ? [] : deps.filter((id) => !known.includes(id));
  const why = title === ""
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

/** Task ids typed as a list ("T1, T2" or "T1 T2"), without blanks or repeats. */
export function splitIds(text) {
  const seen = new Set();
  for (const part of String(text ?? "").split(/[\s,]+/)) {
    if (part !== "") seen.add(part);
  }
  return [...seen];
}

/** The tasks a person may name in "After": every live task's id. */
export function taskIds(state) {
  return (state?.tasks ?? []).filter(isLive).map((task) => task.id);
}
