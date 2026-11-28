// The sidebar's tree and the Graph view, as plain data: the Spaces tree, the
// Agents pane's rows and the Graph's layered rows, all folded from one
// `GET /api/state` answer. No Herdr, no clock of its own, no I/O, so a fixture
// (`test/fixtures/proto.json`) tests the whole shape.
//
// PROTOTYPE (SHPR-18). Two fields are not in the service's answer yet; this is the
// contract the real task must provide, and the prototype's fixture defines it:
//
// - `spaces[]`: `{id, name, folder, branch}`, one per repo/folder, and on each
//   task `space` (a space id) and `phase` (a short group word, may be absent).
//   A state without `spaces` gets one space made from `run`, so a real service
//   still draws.
// - `live_agents[]`: `{id, space, task|null, role, program, state, since_ms}`.
//   `role` is worker | reviewer | coordinator; `task` is null for a coordinator;
//   `state` is working | needs_you | idle | done; `since_ms` is when it entered
//   that state, in ms since the epoch. Headless: an agent has no tab or space.
//
// Shared: the source is `mods/lib/tree.js`; `bin/sync-shared` copies it. Edit the
// source, never a copy.

import { isLive, look } from "./board.js";
import { roleRename, roleTitle, waitLabel } from "./tokens.js";


/** How long a finished agent stays in the Agents pane: 30 minutes. A coordinator stays as long as its space. */
export const FINISHED_KEEP_MS = 30 * 60 * 1000;

/** The task states the sidebar tree shows: the ones a person can act on or watch right now. */
const TREE_STATES = new Set(["working", "needs_you", "ready", "merging"]);

/** The tree's sort: needs you first, then working, then the rest (Q5). */
const TREE_RANK = { needs_you: 0, working: 1, merging: 1, ready: 2 };

/** Whether `agent` still belongs in the Agents pane at `now`: not a finished one older than 30 minutes. */
export function agentKept(agent, now = Date.now()) {
  if (agent?.state !== "done" || agent?.role === "coordinator") return true;
  const ended = Number.isFinite(agent?.ended_ms) ? agent.ended_ms : agent?.since_ms;
  if (!Number.isFinite(ended)) return true;
  return now - ended < FINISHED_KEEP_MS;
}

/** The live agents that are still shown at `now`. */
export function liveAgents(state, now = Date.now()) {
  return (state?.live_agents ?? []).filter((agent) => agentKept(agent, now));
}

/** The words a role is shown as. */
export const ROLE_WORDS = { worker: "Worker", reviewer: "Reviewer", coordinator: "Coordinator" };

/** The words an agent's state is shown as. */
export const AGENT_WORDS = { working: "working", needs_you: "needs you", idle: "idle", done: "done" };

/** How long ago, in the shortest honest words: `now`, `4m`, `2h`, `3d`. */
export function age(sinceMs, now = Date.now()) {
  if (!Number.isFinite(sinceMs)) return "";
  const seconds = Math.max(0, Math.floor((now - sinceMs) / 1000));
  if (seconds < 45) return "now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  return hours < 48 ? `${hours}h` : `${Math.round(hours / 24)}d`;
}

/** An agent's second line: `working · 4m`, and `done · 12m ago` once it is over. */
export function agentLine(agent, now = Date.now()) {
  const word = AGENT_WORDS[agent?.state] ?? String(agent?.state ?? "");
  const since = age(agent?.since_ms, now);
  if (!since) return word;
  return agent?.state === "done" ? `${word} · ${since} ago` : `${word} · ${since}`;
}

/** The status glyph an agent's dot takes: it reuses a task status's shape and tone. */
export function agentLook(agent) {
  if (agent?.state === "working") return look({ derived: { status: "working" } });
  if (agent?.state === "needs_you") return look({ derived: { status: "needs_you" } });
  if (agent?.state === "done") return look({ derived: { status: "ready" } });
  return look({ derived: { status: "queued" } });
}

/** The spaces a state names, or one made from its run so a real service still draws. */
export function spacesOf(state) {
  if (Array.isArray(state?.spaces) && state.spaces.length > 0) return state.spaces;
  const repo = String(state?.run?.repo ?? "").split("/").filter(Boolean).pop() ?? "this repo";
  return [{ id: "run", name: repo, folder: state?.run?.repo ?? "", branch: state?.run?.integration_branch ?? "" }];
}

const trimFolder = (folder) => String(folder ?? "").replace(/\/+$/, "");

/**
 * The served space a picked id stands for. A Herdr space the person made in the
 * served repository's folder (or the one the service says is its own) is listed
 * as a plain `herdr:<id>` row, but its tasks are the served space's: picking it
 * must show them. Any other id comes back unchanged.
 */
export function resolveSpace(state, pickedId, herdrSpaces = []) {
  const id = String(pickedId ?? "");
  if (!id.startsWith("herdr:")) return pickedId;
  const herdrId = id.slice("herdr:".length);
  const served = spacesOf(state);
  const byId = served.find((space) => space.herdr_id && space.herdr_id === herdrId);
  if (byId) return byId.id;
  const herdr = (herdrSpaces ?? []).find((space) => space?.id === herdrId);
  if (!herdr) return pickedId;
  const folder = trimFolder(herdr.folder);
  const same = served.find((space) => folder !== "" && trimFolder(space.folder) === folder);
  return same ? same.id : pickedId;
}

/**
 * Whether a picked id still names a space: a `herdr:<id>` whose Herdr space was
 * closed does not, and is dropped (`null`) rather than drawn as a raw id or used
 * to scope the panes to nothing. A served id and an unknowable case (Herdr's list
 * is empty) are kept; `scopeToSpace` already falls back for a served id it lacks.
 */
export function livePick(pickedId, herdrSpaces = []) {
  if (pickedId === null || pickedId === undefined) return null;
  const id = String(pickedId);
  const list = Array.isArray(herdrSpaces) ? herdrSpaces : [];
  if (!id.startsWith("herdr:") || list.length === 0) return pickedId;
  const herdrId = id.slice("herdr:".length);
  return list.some((space) => space?.id === herdrId) ? pickedId : null;
}

/**
 * What `spaces.picked` should be now, so the panes follow the window's own space.
 *
 * - none picked ("All spaces") stays none;
 * - a pick whose Herdr space was closed moves to the space the window shows;
 * - when the window's space changes (`previousActive` was another), a pick that
 *   does not stand for the new one follows it.
 *
 * Returns the id to share, the same as `pickedId` when nothing changes.
 */
export function followPick(state, pickedId, herdrSpaces = [], previousActive = null) {
  if (pickedId === null || pickedId === undefined) return null;
  const list = Array.isArray(herdrSpaces) ? herdrSpaces : [];
  const active = list.find((space) => space?.active) ?? null;
  const mine = active ? `herdr:${active.id}` : null;
  if (livePick(pickedId, list) === null) return mine;
  if (active && previousActive !== null && previousActive !== active.id) {
    const same = resolveSpace(state, pickedId, list) === resolveSpace(state, mine, list);
    if (!same) return mine;
  }
  return pickedId;
}

/** The name to draw for a picked id: the served space's, else Herdr's, never a raw id. */
export function pickedName(state, pickedId, herdrSpaces = []) {
  const resolved = resolveSpace(state, pickedId, herdrSpaces);
  const served = spacesOf(state).find((space) => space.id === resolved);
  if (served) return served.herdr_name ?? served.name;
  const herdrId = String(pickedId).replace(/^herdr:/, "");
  const herdr = (herdrSpaces ?? []).find((space) => space?.id === herdrId);
  // Never a raw `sp…` id: a closed space's name is gone, so say what it is.
  if (herdr?.name) return String(herdr.name);
  return String(pickedId).startsWith("herdr:") ? "This space" : String(pickedId).replace(/^sp-/, "");
}

/**
 * The state with only one space's tasks (`spaceId` null: all of them), for the
 * Tasks list. A task with no space belongs to the only space there is.
 */
export function scopeToSpace(state, pickedId, herdrSpaces = []) {
  if (!state || pickedId === null || pickedId === undefined) return state;
  const list = spacesOf(state);
  const spaceId = resolveSpace(state, pickedId, herdrSpaces);
  if (String(spaceId).startsWith("herdr:")) return { ...state, tasks: [] };
  if (!list.some((space) => space.id === spaceId)) return state;
  const tasks = (state.tasks ?? []).filter((task) => (task.space ?? (list.length === 1 ? list[0].id : null)) === spaceId);
  return { ...state, tasks };
}

/** The task a state's agent works on, or null for a coordinator. */
function taskOf(state, agent) {
  return (state?.tasks ?? []).find((task) => task.id === agent?.task) ?? null;
}

/** The live agents of one task, in the order the service gave them. */
export function agentsOfTask(state, taskId, now = Date.now()) {
  return liveAgents(state, now).filter((agent) => agent.task === taskId);
}

/**
 * A task row's dim second line: who is on it and where it stands, then progress
 * and time to go when known — `claude · working · R#1 · 6m`, `codex · reviewing`,
 * `needs answer · 2m ago`, `ready to approve`. Time to go is not in the service's answer yet, so it is not drawn.
 */
export function taskLine(state, task, now = Date.now()) {
  const agents = agentsOfTask(state, task.id, now).filter((agent) => agent.state !== "done");
  const worker = agents.find((agent) => agent.role === "worker");
  const reviewer = agents.find((agent) => agent.role === "reviewer");
  const status = task.derived?.status;
  const attempt = (task.attempts ?? []).at(-1);
  const rounds = attempt?.reviews?.length ?? 0;
  const parts = [];
  if (status === "needs_you") {
    parts.push(waitLabel(task));
    const since = age(worker?.since_ms, now);
    if (since) parts.push(since === "now" ? "now" : `${since} ago`);
    return parts.join(" · ");
  } else if (status === "ready") {
    return "ready to approve";
  } else if (reviewer) {
    // The person's own name for the reviewer, when they gave one: `codex · QA reviewing`.
    const qa = roleRename(state, "reviewer");
    parts.push(reviewer.program, qa ? `${qa} reviewing` : "reviewing");
  } else if (status === "merging") {
    return "merging";
  } else {
    if (worker?.program) parts.push(worker.program);
    const dev = worker ? roleRename(state, "worker") : null;
    const word = status === "working" || worker?.state === "working" ? "working" : String(status ?? "").replace("_", " ");
    parts.push(dev ? `${dev} ${word}` : word);
  }
  if (rounds > 0) parts.push(`R#${rounds}`);
  const since = age(worker?.since_ms ?? reviewer?.since_ms, now);
  if (since) parts.push(since);
  const pct = look(task).pct;
  if (pct !== null && pct !== undefined) parts.push(`${pct}%`);
  return parts.filter(Boolean).join(" · ");
}

/**
 * The tasks of one space, ordered like the board (what needs the person first),
 * each as the row the tree draws.
 *
 * - `sub`: for a working task its agent and progress (`claude · 62%`), else null.
 * - `last`: whether it is the space's last task, which gives it └─ not ├─.
 */
export function treeTasks(state, space, spaceCount = 1, now = Date.now()) {
  const rank = (task) => TREE_RANK[task?.derived?.status] ?? 3;
  const withAgent = new Set(
    liveAgents(state, now)
      .filter((agent) => agent.task && agent.state !== "done")
      .map((agent) => agent.task),
  );
  const mine = (state?.tasks ?? [])
    .filter(isLive)
    .filter((task) => TREE_STATES.has(task?.derived?.status) || withAgent.has(task.id))
    .filter((task) => task?.derived?.status !== "done")
    .filter((task) => (task.space ?? (spaceCount === 1 ? space.id : null)) === space.id)
    .sort((a, b) => rank(a) - rank(b) || String(a.id).localeCompare(String(b.id)));
  return mine.map((task, at) => {
    const shape = look(task);
    return {
      id: task.id,
      title: String(task.title ?? ""),
      status: task.derived?.status ?? "to_do",
      // A needs-you row the coordinator settles itself is not the person's.
      forYou: task.derived?.for_you !== false,
      glyph: shape.glyph,
      tone: shape.tone,
      pct: shape.pct,
      sub: taskLine(state, task, now),
      last: at === mine.length - 1,
    };
  });
}

/**
 * The Spaces section: one entry per space with its branch, its tasks and the two
 * numbers a row's badge would show.
 */
export function spaceTree(state, herdrSpaces = [], now = Date.now()) {
  const list = rebindSpaces(spacesOf(state), herdrSpaces);
  const own = list.map((space) => {
    const tasks = treeTasks(state, space, list.length, now);
    return {
      id: space.id,
      // The row carries Herdr's real space (id and name) when the service reports it.
      name: space.herdr_name ?? space.name,
      herdr: space.herdr_id ?? null,
      branch: space.branch ?? "",
      folder: space.folder ?? "",
      plain: false,
      tasks,
      needs: tasks.filter((task) => task.status === "needs_you" && task.forYou !== false).length,
      ready: tasks.filter((task) => task.status === "ready").length,
    };
  });
  return inWindowOrder([...own, ...plainSpaces(list, herdrSpaces)], list, herdrSpaces);
}

/**
 * Served spaces whose Herdr space was closed, bound to the Herdr space the person
 * made for the same folder since (SHPR-80). The service keeps a space by its
 * folder, so a folder closed and added again is the same served space with a new
 * Herdr id: left as it was, the row would name the closed space and the new one
 * would show beside it as a second, plain row.
 */
export function rebindSpaces(served, herdrSpaces = []) {
  const live = Array.isArray(herdrSpaces) ? herdrSpaces.filter((space) => space?.id) : [];
  if (live.length === 0) return served;
  const taken = new Set(served.map((space) => space.herdr_id).filter((id) => live.some((h) => h.id === id)));
  return served.map((space) => {
    if (!space.herdr_id || live.some((h) => h.id === space.herdr_id)) return space;
    const folder = trimFolder(space.folder);
    const home = live.find((h) => folder !== "" && trimFolder(h.folder) === folder && !taken.has(h.id));
    if (!home) return space;
    taken.add(home.id);
    return { ...space, herdr_id: home.id, herdr_name: home.name ?? space.herdr_name };
  });
}

/**
 * The rows in the window's own space order (Herdr's `spaces()`), never the
 * service's: a served row sits where its Herdr space is (`herdr` id, else the
 * `shprd · <repo>` name), so picking a space or a task changing state never moves
 * a row. A row with no Herdr space (the service is up, the window has none yet)
 * goes last, in the service's order.
 */
export function inWindowOrder(rows, served, herdrSpaces = []) {
  const order = new Map((herdrSpaces ?? []).map((space, at) => [space?.id, at]));
  const byName = new Map((herdrSpaces ?? []).map((space, at) => [space?.name, at]));
  const slot = (row, at) => {
    if (row.plain) return order.get(row.herdr) ?? rows.length + at;
    const named = byName.get(`shprd · ${row.name}`) ?? byName.get(row.name);
    return order.get(row.herdr) ?? (row.herdr ? undefined : named) ?? rows.length + at;
  };
  return rows
    .map((row, at) => ({ row, key: slot(row, at), at }))
    .sort((a, b) => a.key - b.key || a.at - b.at)
    .map((entry) => entry.row);
}

/**
 * A row's second line, the same rule for every row: its folder, then the branch
 * after it only when it is not the usual one (Shepherdr's `gitTail` hides
 * main/master). A row with no folder shows the unusual branch alone.
 */
export function subtitle(space) {
  const branch = String(space?.branch ?? "").trim();
  const unusual = branch && branch !== "main" && branch !== "master" ? branch : "";
  return [space?.folder ?? "", unusual].filter(Boolean).join(" · ");
}

/**
 * The spaces Herdr has that the service does not serve, as plain rows: a name
 * and a folder, no tree. Only the exact Herdr space a served row stands for
 * (`herdr_id`) is left out; every other space, including the person's own in the
 * same folder, shows. A service that reports no `herdr_id` (the host was not
 * asked) falls back to hiding the space named `shprd · <repo>`.
 */
export function plainSpaces(served, herdrSpaces = []) {
  const ids = new Set(served.map((space) => space.herdr_id).filter(Boolean));
  const names = new Set(served.filter((space) => !space.herdr_id).map((space) => `shprd · ${space.name}`));
  return (herdrSpaces ?? [])
    .filter((space) => space?.id && !ids.has(space.id) && !names.has(space.name))
    .map((space) => ({
      id: `herdr:${space.id}`,
      herdr: space.id,
      name: String(space.name ?? space.id),
      branch: "",
      folder: space.folder ?? "",
      plain: true,
      active: Boolean(space.active),
      tasks: [],
      needs: 0,
      ready: 0,
    }));
}

/**
 * The Agents pane's rows. Ungrouped: one row per live agent, needs-you first.
 * Grouped: a `head` row per space, then its agents.
 *
 * A row is `{kind: "agent", id, name, role, program, line, glyph, tone, pct,
 * task, space}`; `name` is the task's `id title`, or the space's name for a
 * coordinator.
 */
export function agentRows(state, { grouped = false, now = Date.now() } = {}) {
  const spaces = new Map(spacesOf(state).map((space) => [space.id, space]));
  const order = { needs_you: 0, working: 1, done: 2, idle: 3 };
  const rows = liveAgents(state, now).map((agent) => {
    const task = taskOf(state, agent);
    const shape = agentLook(agent);
    return {
      kind: "agent",
      id: agent.id,
      name: task ? `${task.id} ${task.title}` : (spaces.get(agent.space)?.name ?? agent.space ?? ""),
      role: roleTitle(state, agent.role),
      program: String(agent.program ?? ""),
      line: agentLine(agent, now),
      glyph: shape.glyph,
      tone: shape.tone,
      pct: shape.pct,
      state: agent.state,
      task: task?.id ?? null,
      title: task?.title ?? "",
      space: agent.space ?? null,
    };
  });
  const sorted = rows.slice().sort((a, b) => (order[a.state] ?? 9) - (order[b.state] ?? 9));
  if (!grouped) return sorted;
  const out = [];
  for (const [id, space] of spaces) {
    const own = sorted.filter((row) => row.space === id);
    if (own.length === 0) continue;
    out.push({ kind: "head", id: `head-${id}`, name: space.name, count: own.length });
    out.push(...own);
  }
  return out;
}

/**
 * The Graph view's rows: the live tasks grouped by `phase`, each phase's tasks a
 * layered tree (a task hangs under its last dependency inside the same phase),
 * and a right-aligned word per task: `done`, `working`, `needs you`, `ready`, or
 * `waits SHPR-23` for what is held back by an unfinished dependency.
 *
 * `guide` is the text before the id: the │ ├─ └─ lines of its ancestors.
 */
export function graphRows(state, pickedId = null, herdrSpaces = []) {
  const list = spacesOf(state);
  const spaceId = pickedId === null ? null : resolveSpace(state, pickedId, herdrSpaces);
  const tasks = (state?.tasks ?? [])
    .filter(isLive)
    .filter((task) => spaceId === null || (task.space ?? (list.length === 1 ? list[0].id : null)) === spaceId);
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const phases = [];
  for (const task of tasks) {
    const name = task.phase ?? "Tasks";
    let phase = phases.find((entry) => entry.name === name);
    if (!phase) phases.push((phase = { name, tasks: [] }));
    phase.tasks.push(task);
  }
  const out = [];
  for (const phase of phases) {
    const ids = new Set(phase.tasks.map((task) => task.id));
    const parentOf = (task) => [...(task.deps ?? [])].reverse().find((dep) => ids.has(dep)) ?? null;
    const kids = new Map();
    const roots = [];
    for (const task of phase.tasks) {
      const parent = parentOf(task);
      if (parent === null) roots.push(task);
      else kids.set(parent, [...(kids.get(parent) ?? []), task]);
    }
    const done = phase.tasks.filter((task) => task.derived?.status === "done").length;
    out.push({ kind: "phase", name: phase.name.toUpperCase(), done, total: phase.tasks.length });
    const walk = (task, prefix, last, top) => {
      const shape = look(task);
      const open = (task.deps ?? []).filter((dep) => byId.get(dep) && byId.get(dep).derived?.status !== "done");
      const at = task.derived?.status;
      const words =
        at === "queued" || at === "to_do"
          ? open.length > 0
            ? `waits ${open.join(" ")}`
            : "queued"
          : at === "needs_you"
            ? waitLabel(task)
            : at;
      out.push({
        kind: "task",
        id: task.id,
        title: String(task.title ?? ""),
        guide: top ? "" : `${prefix}${last ? "└─ " : "├─ "}`,
        status: task.derived?.status ?? "to_do",
        glyph: shape.glyph,
        tone: shape.tone,
        pct: shape.pct,
        word: task.pending?.label ?? words,
        // The dependency a waiting task is held by, so the view can dim it.
        waits: !task.pending?.label && words.startsWith("waits"),
      });
      const next = top ? "" : `${prefix}${last ? "   " : "│  "}`;
      const own = kids.get(task.id) ?? [];
      own.forEach((kid, at) => walk(kid, next, at === own.length - 1, false));
    };
    roots.forEach((task) => walk(task, "", true, true));
  }
  return out;
}

/**
 * The Herdr space a task's tab belongs in: the task's repo space. `servedId` is
 * the served space id on the task (`task.space`) or a `herdr:<id>` row; it is
 * matched by the served space's `herdr_id`, then by folder. Null when no Herdr
 * space is known (the caller then makes the tab in the shown space).
 */
export function herdrSpaceOf(state, servedId, herdrSpaces = []) {
  const list = Array.isArray(herdrSpaces) ? herdrSpaces : [];
  const id = String(servedId ?? "");
  if (id.startsWith("herdr:")) return id.slice("herdr:".length);
  if (list.some((space) => space?.id === id)) return id;
  const served = spacesOf(state).find((space) => space.id === id);
  if (!served) return null;
  if (served.herdr_id && list.some((space) => space?.id === served.herdr_id)) return served.herdr_id;
  const folder = trimFolder(served.folder);
  const same = list.find((space) => folder !== "" && trimFolder(space?.folder) === folder);
  return same ? same.id : null;
}
