// The tab bar's reading of a space: one pill per tab, with the agent that runs
// in it and the state it is in, plus the side panel's buttons.
//
// Two facts meet here, and the module only joins them:
//
// - shprd's own tab titles: `<agent>·<task>` for the task's worker
//   (`crates/shprd/src/cmd/spawn.rs`, `worker_title`) and
//   `<agent>·<task>·review` for its reviewer (`crates/shprd/src/cmd/review.rs`,
//   `reviewer_title`). Those are the only two titles shprd writes, and there are
//   only two roles to read from them: a backup that takes a task over does it in
//   the worker's own tab, through the same `open_agent` path with the same
//   `<agent>·<task>` title (`handoff.rs`), so its tab reads as the worker's with
//   the backup agent's name on it — which is what is running there. A tab whose
//   title is neither shape is a person's own tab: it still gets a pill, with no
//   agent and no task claimed.
// - Herdr's live view of a tab: the most urgent agent status in it
//   (`working | blocked | done | idle`), and its panes. That is the truth about
//   the terminal; shprd's snapshot is the truth about the task, and it is what a
//   pill says when the host has no status to report (a shell, a program that
//   reports nothing).
//
// Pure: a snapshot, a `SpaceInfo` and `PaneInfo[]` in; rows out. The mod draws
// them, and the unit test drives them with fixtures.
//
// Ported from Shepherdr's tab bar (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/main.js` (the pill row and the panel's buttons),
//   `core/tasks.js` (`STATUS`, the words a tab's dot stands for)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/tabstrip.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { roleTitle, statusColor, status as statusOf } from "./tokens.js";

/** The separator shprd's tab titles are built from (`worker_title`). */
export const SEP = "·";

/** The words a role is called by. */
export const ROLE = {
  worker: "Worker",
  reviewer: "Reviewer",
};

/**
 * The role a title's last segment names.
 *
 * `review` is the one suffix shprd writes (`reviewer_title`, `·review`); the
 * worker's title has no suffix at all (`worker_title`). A segment this build
 * does not know is read as the worker's, because that is the title a worker
 * tab — and a backup that took one over — really carries, and inventing a
 * third role for a word nobody writes would be a claim the run does not back.
 */
export const SUFFIX = { review: "reviewer", worker: "worker" };

/** Herdr's agent states, as a pill names and colours them. */
export const LIVE = {
  working: { word: "Working", tone: "amber", color: "working", glyph: "pie" },
  blocked: { word: "Needs you", tone: "red", color: "waiting", glyph: "dot" },
  done: { word: "Finished", tone: "green", color: "done", glyph: "tick" },
  idle: { word: "Idle", tone: "dim", glyph: "ring" },
};

/**
 * What a tab's title says: the agent, the task and the role.
 *
 * `titled: false` means the title is not one shprd gives a tab, so the pill
 * shows the title alone. A reviewer's title ends in `review` (`reviewer_title`);
 * anything else — the worker's own `<agent>·<task>`, and a backup's, which is
 * the same title with another agent in it — is the worker's.
 */
export function parseTabName(name) {
  const text = String(name ?? "").trim();
  const parts = text.split(SEP);
  if (parts.length < 2 || parts[0] === "" || parts[1] === "") {
    return { titled: false, agent: null, task: null, role: null, name: text };
  }
  const suffix = parts.length > 2 ? parts[2].toLowerCase() : "worker";
  const role = SUFFIX[suffix] ?? "worker";
  return {
    titled: true,
    agent: parts[0],
    task: parts[1],
    role,
    name: text,
  };
}

/**
 * The program a tab shows, from the app's own table for its panes.
 *
 * Shepherdr's rule (`mods/tabs-tasks/main.js:155`): the first pane in the tab that
 * reports as an agent, else the tab's first pane at all. That is what gives a pill
 * its icon: an agent's own glyph when one runs there, else whatever the shell is.
 * A tab with no pane reports nothing, and the pill draws no icon rather than
 * guessing at one.
 */
export function tabProgram(panes, tabId) {
  const mine = (panes ?? []).filter((pane) => pane?.tab === tabId);
  const agent = mine.find((pane) => pane?.program?.agent);
  return (agent ?? mine[0])?.program ?? null;
}

/** The most urgent agent status among a tab's panes, or null when none reports. */
export function paneState(panes, tabId) {
  const mine = (panes ?? []).filter((pane) => pane?.tab === tabId);
  for (const word of ["blocked", "done", "working", "idle"]) {
    if (mine.some((pane) => pane.status === word)) return word;
  }
  return null;
}

/** One task of the snapshot, by id, or null. */
export function taskOf(state, id) {
  if (!id) return null;
  return (state?.tasks ?? []).find((task) => task?.id === id) ?? null;
}

/**
 * A pill's state: the host's word when it has one, else the task's own.
 *
 * A host that reports nothing is not silence about the task — shprd's rules
 * still know where the task stands — so the fallback is the snapshot's derived
 * word, and a tab shprd did not title reads `Idle` rather than inventing one.
 */
export function tabState(tab, panes, state) {
  const live = paneState(panes, tab?.id);
  if (live) return { status: live, ...LIVE[live], from: "host" };
  const task = taskOf(state, parseTabName(tab?.name).task);
  if (!task) return { status: "idle", ...LIVE.idle, from: "none" };
  const look = statusOf(task.derived?.status);
  return {
    status: task.derived?.status ?? "to_do",
    word: look.word,
    tone: look.tone,
    color: statusColor(task),
    glyph: look.glyph,
    from: "shprd",
  };
}

/**
 * What a pill is called: the title shprd gave the tab, else the name someone
 * gave it, else what runs in it (Shepherdr's `tabName`, `core/menus.js:106`).
 *
 * Herdr names a tab nobody named by its position ("1"), which is not a name a
 * person reads on a pill — the program's own name is ("zsh", "Claude Code"), and
 * "Terminal" when even that is unknown.
 */
export function tabLabel(tab, named, program) {
  if (named?.titled) return named.name;
  const text = String(tab?.name ?? named?.name ?? "").trim();
  // Herdr's own names for a tab nobody named are its position ("1") and, on
  // older builds, "tb3"; neither is a name a pill should wear.
  const automatic = text === "" || /^(tb)?\d+$/.test(text);
  if (!automatic) return text;
  return program?.name || "Terminal";
}

/**
 * Every pill of `space`, in the host's order.
 *
 * A pill carries what the mod draws and what a test asserts: the tab's id, the
 * agent and role its title names, the task's id and its derived word, and
 * whether it is the shown tab. `active` is the host's own answer, so a click
 * that missed never moves the highlight here.
 */
export function tabRows(space, panes, state, coordinator = null, icons = {}) {
  const tabs = space?.tabs ?? [];
  return tabs.flatMap((tab, index) => {
    // Another product's Assistant tab (Shepherdr's chat pane) is not shprd's to
    // show: its message would reach that product's agent, not the coordinator.
    if (foreignTab(tab, panes)) return [];
    const named = parseTabName(tab?.name);
    const shape = tabState(tab, panes, state);
    const task = taskOf(state, named.task);
    const program = tabProgram(panes, tab?.id) ?? taskTabProgram(panes, tab?.id, state, icons);
    // The coordinator's tab is its role's, not the repository's: the role's display
    // name from the settings, and the icon of the harness the role runs now.
    const mine = shprdCoordinatorTab(tab);
    return [{
      id: tab.id,
      index,
      name: named.name,
      label: mine ? roleTitle(state, "coordinator") : tabLabel(tab, named, program),
      // Whether a person named it (`tab.rename`, `tab.new {name}`): the rename
      // prompt's field holds the name only when there is one.
      named: Boolean(tab.named),
      agent: named.agent,
      role: named.role,
      roleWord: named.role ? roleTitle(state, named.role) : null,
      task: named.task,
      title: String(task?.title ?? ""),
      active: Boolean(tab.active),
      status: shape.status,
      word: shape.word,
      tone: shape.tone,
      color: shape.color,
      glyph: shape.glyph,
      from: shape.from,
      needs: shape.tone === "red",
      // The badge's two numbers: one task is one tab, so a pill's count is at most
      // one of each — what needs the person (red) and what is ready (green). They
      // are counts, not flags, because the badge is the one that takes two.
      need: shape.tone === "red" ? 1 : 0,
      ready: shape.tone === "green" ? 1 : 0,
      // The pill's icon: the program's own, from the app's table (`name` for the
      // tab's own title when nobody named it).
      icon: mine ? (coordinator?.icon ?? null) : (program?.icon ?? null),
      tint: mine ? Boolean(coordinator?.tint) : Boolean(program?.tint),
      program: program?.name ?? null,
      zoomed: Boolean(tab.zoomed),
      panes: Number.isInteger(tab.panes) ? tab.panes : 0,
    }];
  });
}

/**
 * The harness of the agent on a task right now: the reviewer's while the task is
 * in review, else the worker's. The program comes from the task's current attempt
 * (`agent`) and its review (`reviews[].reviewer`); a live reviewer agent in the
 * snapshot is the same answer, sooner. Null when the task has none.
 */
export function taskHarness(task, state) {
  if (!task) return null;
  const live = (state?.live_agents ?? []).find(
    (agent) => agent?.task === task.id && agent?.role === "reviewer" && agent?.state !== "done",
  );
  if (live?.program) return live.program;
  const latest = (task.attempts ?? [])[(task.attempts ?? []).length - 1];
  const reviews = latest?.reviews ?? [];
  const settled = (latest?.gates ?? []).some((gate) => gate?.gate === "review");
  if (reviews.length > 0 && !settled) return reviews[reviews.length - 1]?.reviewer ?? null;
  return latest?.agent ?? null;
}

/**
 * What a task's chat tab shows as its program: the icon of the harness on that
 * task (`taskHarness`), from the app's own table of agents (`icons`, harness id to
 * `{icon, tint}`). It is read each paint, so it changes with the task's state.
 */
export function taskTabProgram(panes, tabId, state, icons = {}) {
  const chat = (panes ?? []).find(
    (pane) => pane?.tab === tabId && pane?.mod === "shprd-chat" && pane?.props?.space && pane.props.space !== "coordinator",
  );
  if (!chat) return null;
  const id = taskHarness(taskOf(state, chat.props.space), state);
  const found = id ? icons[id] : null;
  return found ? { name: id, icon: found.icon, tint: Boolean(found.tint) } : null;
}

/**
 * The harness the coordinator role runs, with its icon: `roles.coordinator.agent`
 * from the settings document (`GET /api/settings`) looked up in `icons`, the
 * harnesses' icons by id (`{ icon, tint }`, from Herdr's known agents). Null when
 * the role has no program or the harness has no icon.
 */
export function coordinatorProgram(settings, icons) {
  const row = (settings?.values ?? []).find((entry) => entry?.key === "roles.coordinator.agent");
  const id = typeof row?.value === "string" ? row.value : null;
  const found = id ? icons?.[id] : null;
  return found?.icon ? { id, icon: found.icon, tint: Boolean(found.tint) } : null;
}

/** The mod Shepherdr's Assistant chat pane is; shprd's own is `shprd-chat`. */
export const FOREIGN_CHAT_MOD = "chat";

/** Whether a tab is another product's Assistant (a pane of Shepherdr's chat mod). */
export function foreignTab(tab, panes) {
  return (panes ?? []).some((pane) => pane?.tab === tab?.id && pane?.mod === FOREIGN_CHAT_MOD);
}

/** Whether a tab is the one shprd made for a space's coordinator (`shprd-coordinator-<space>`). */
export function shprdCoordinatorTab(tab) {
  return /(^|\/)shprd-coordinator(-|$)/.test(String(tab?.id ?? ""));
}

/**
 * The tab to select so a shprd space's center shows shprd's coordinator and
 * nothing else (SHPR-79), or `null` when nothing needs to move.
 *
 * Shepherdr's tab bar adds an "Assistant" tab to every space it has not seen and
 * selects it, and the layout's center shows the active tab. Hiding the pill does
 * not stop that tab from being the one shown, so the bar moves the selection to
 * the coordinator's tab. A space with no shprd coordinator tab is not shprd's,
 * and a Shepherdr Assistant in it (or in `shprd · shprd`, where the orchestrator
 * runs) is left exactly as it is: nothing is closed, killed or renamed here.
 */
export function strayTab(space, panes) {
  const tabs = space?.tabs ?? [];
  const active = tabs.find((tab) => tab?.active);
  if (!active || !foreignTab(active, panes)) return null;
  const own = tabs.find((tab) => shprdCoordinatorTab(tab) && !foreignTab(tab, panes));
  return own?.id ?? null;
}

/**
 * Shepherdr's `coordinator.seen` list with shprd's own spaces added, or `null`
 * when it already holds them (or Shepherdr has not started the list: it will
 * note every space then). A space in that list is one Shepherdr never gives an
 * Assistant to when its tab bar next draws, which is the way an Assistant tab
 * got into a space made while the shprd layout was shown.
 */
export function seenWith(seen, spaces) {
  if (!Array.isArray(seen)) return null;
  const add = (spaces ?? [])
    .filter((space) => !space?.machine && !String(space?.id ?? "").includes("/"))
    .filter((space) => String(space?.name ?? "").startsWith("shprd · "))
    .map((space) => space.id)
    .filter((id) => !seen.includes(id));
  return add.length > 0 ? [...seen, ...add] : null;
}

/**
 * What a left release on a pill does: `{select: id}` for a plain click, or
 * `{move: id, index}` for a drag from another pill (the host counts from one).
 *
 * **It compares tab ids, never positions.** A press remembers the pill it went
 * down on; a position can shift between press and release (a sweep closing an
 * ended task's tab moves every pill after it), and a click then looked like a
 * drag to the wrong place, so the tab never got selected. A press with no
 * remembered pill, or one whose tab is gone, is a click on the pill released on.
 */
export function pillRelease(pressed, row, rows = []) {
  const stale = !pressed || !rows.some((entry) => entry?.id === pressed.id);
  if (stale || pressed.id === row.id) return { select: row.id };
  return { move: pressed.id, index: row.index + 1 };
}

/** The pills' counts, for a heading or a test. */
export function pillCounts(rows) {
  const tally = { total: 0, working: 0, needs: 0, ready: 0 };
  for (const row of rows ?? []) {
    tally.total += 1;
    if (row.tone === "red") tally.needs += 1;
    else if (row.tone === "amber") tally.working += 1;
    else if (row.tone === "green") tally.ready += 1;
  }
  return tally;
}

/** The side panel's stacks, in the order the bar draws their buttons. */
export const PANELS = [
  { id: "files", word: "Files" },
  { id: "changes", word: "Changes" },
  { id: "tasks", word: "Tasks" },
];

/**
 * The side panel's buttons, with the counts each one's stack would show.
 *
 * The Tasks button carries shprd's two counts the way Shepherdr's did — what
 * needs the person, then what is ready for them. There is no Asks button: the
 * cards a program raises are in its task's chat thread, and the Tasks button's red
 * count is how a person knows one is waiting. Settings has no button of its own: it is a dialog over the window
 * (`openSettings()` in `lib/panel.js`), opened from the sidebar's footer, exactly
 * as Shepherdr's is, so nothing here names it.
 */
export function panelButtons(state, shown = "tasks") {
  const tally = { needs: 0, ready: 0 };
  for (const task of state?.tasks ?? []) {
    if (task?.status === "dropped") continue;
    if (task?.derived?.status === "needs_you") {
      // What the coordinator settles itself is not counted as the person's.
      if (task?.derived?.for_you !== false) tally.needs += 1;
    }
    else if (task?.derived?.status === "ready") tally.ready += 1;
  }
  if (state?.integration_pr) tally.needs += 1;
  return PANELS.map((panel) => ({
    ...panel,
    active: panel.id === shown,
    needs: panel.id === "tasks" ? tally.needs : 0,
    ready: panel.id === "tasks" ? tally.ready : 0,
  }));
}

/** The layout node called `id` under `node`, or null. */
function findNode(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = findNode(child, id);
    if (found) return found;
  }
  return null;
}

/**
 * The side panel's stack that is showing, read from the layout itself, or null
 * when the panel is hidden.
 *
 * The layout is the truth: a row elsewhere (a sidebar task, Jump, a key) moves
 * the panel without telling the bar, so a flag the bar kept would drift.
 */
export function shownPanel(root) {
  const panel = findNode(root, "panel");
  if (!panel || panel.hidden) return null;
  const body = findNode(root, "panel-body");
  const active = body?.active ?? null;
  return PANELS.some((entry) => entry.id === active) ? active : null;
}

/**
 * Per-space panel memory: what the panel showed in each space, put back when the
 * space is shown again.
 *
 * `memory` is a Map of space id to a stack id (or null for hidden), `last` the
 * space seen on the previous look and `current` what the layout shows now.
 * While the space stays the same the look is recorded; when it changes, nothing
 * is recorded (the layout still shows the old space's panel) and `restore` says
 * what to put back: `undefined` for a space not seen before (leave the panel
 * alone), else a stack id or null.
 */
export function rememberPanel(memory, last, spaceId, current) {
  if (!spaceId) return { last, restore: undefined };
  if (spaceId === last) {
    memory.set(spaceId, current);
    return { last: spaceId, restore: undefined };
  }
  return { last: spaceId, restore: memory.has(spaceId) ? memory.get(spaceId) : undefined };
}
