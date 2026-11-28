// The run document a graph is drawn from: dagr's v3 contract, as shprd writes
// it (`<repo>/.dagr/run.json`), read by the browser.
//
// The document is not one of the service's DTOs — `shprd-views` builds it as
// JSON and `dagr check --strict` is the only judge of it — so these types are
// the TS mirror of the contract the projector writes, not generated ones. They
// cover the fields a drawing reads; a field this build has never heard of is
// ignored, exactly as the socket ignores an unknown message kind, so a newer
// producer still draws in an older page.
//
// What the projection states (and this mirror keeps) is in
// `crates/shprd-views/src/dagr.rs`: tasks carry their dagr state, their
// dependencies, and one record per attempt; an attempt's outcome carries an
// evidence tier (`verified` when a gate passed, `reported` from a report,
// `asserted` when neither did).

/** A dagr task state. `state` is kept as a string on the wire, see [`taskTone`]. */
export type RunTaskState =
  | "queued"
  | "working"
  | "review"
  | "blocked"
  | "done"
  | "failed"
  | "rejected"
  | "canceled"
  | "settled_unverified";

/** A dagr attempt state. */
export type RunAttemptState =
  | "queued"
  | "working"
  | "done"
  | "failed"
  | "rejected"
  | "lost"
  | "settled_unverified";

/** How strongly an outcome is vouched for: the contract's §4 tiers. */
export type RunEvidence = "verified" | "reported" | "heuristic" | "asserted";

/** The run's identity, as the document's header states it. */
export interface RunHeader {
  /** Run identifier. */
  id: string;
  /** The document's title: `<repo basename> · <run id>`. */
  title: string;
  /** When the run started, RFC 3339 UTC. */
  started_at: string;
}

/** One project scope, as the document lists it. */
export interface RunProject {
  /** Project id: a top-level path component. */
  id: string;
  /** The project's title. */
  title: string;
}

/** Why an attempt exists. */
export interface RunCause {
  /** `initial`, `sent_back`, `gate_failed`, `followup` or `superseded`. */
  type: string;
}

/** What settled an attempt. */
export interface RunOutcome {
  /** The attempt's result, a dagr attempt state. */
  result: string;
  /** The evidence tier. */
  evidence: RunEvidence;
}

/** One attempt at a task. */
export interface RunAttempt {
  /** `T·aN` style identity. */
  id: string;
  /** Attempt number, from 1. */
  n: number;
  /** Why the attempt exists. */
  cause: RunCause;
  /** The agent that ran it. */
  actor: string;
  /** The model it ran on, when one was recorded. */
  model?: string;
  /** Where the attempt stands. */
  state: string;
  /** When it started, RFC 3339 UTC. */
  started_at: string;
  /** When it settled, RFC 3339 UTC; absent while it runs. */
  ended_at?: string;
  /** What settled it, absent while it runs. */
  outcome?: RunOutcome;
}

/** One task: the work item and its attempts. */
export interface RunTask {
  /** Stable task id. */
  id: string;
  /** One-line title. */
  title: string;
  /** `impl`, `review` or `docs`. */
  kind: string;
  /** The project it is drawn under; absent means the run root. */
  project?: string;
  /** Its dagr state. */
  state: string;
  /** Tasks that must be done first. */
  deps: string[];
  /** What must be true when it is done. */
  criteria: string;
  /** Who is on it now. */
  owner?: string;
  /** Who a blocked task waits for. */
  unblock?: string;
  /** Why it reads the way it does, when the projection added a word. */
  note?: string;
  /** Its attempts, in order. */
  attempts: RunAttempt[];
}

/** One entry of the document's event tail. */
export interface RunEvent {
  /** When it happened, RFC 3339 UTC. */
  at: string;
  /** `attempt_started`, `attempt_settled` or `note`. */
  type: string;
  /** The task it concerns, when it concerns one. */
  task?: string;
  /** The attempt it concerns, as `T·aN`. */
  attempt?: string;
  /** One word for what happened. */
  detail: string;
}

/** A dagr v3 run document: what `dagr view` draws, and what this page draws. */
export interface RunDocument {
  /** The contract version; 3 as shprd writes it. */
  dagr: number;
  /** The run's identity. */
  run: RunHeader;
  /** When the document was written, RFC 3339 UTC. */
  generated_at: string;
  /** The run's project scopes. */
  projects: RunProject[];
  /** Every task, in id order. */
  tasks: RunTask[];
  /** The last events, newest last. */
  events: RunEvent[];
}

/** What reading a document gave: the document, or why it is not one. */
export type ParseResult =
  | { ok: true; document: RunDocument }
  | { ok: false; problem: string };

/**
 * Reads one answer from `GET /api/graph/run` as a run document.
 *
 * The service has already refused a file that is not a document, so this is the
 * page's own check of the fields it draws: a document missing a task's id or a
 * dependency list would draw a graph that lies about the run, and a person
 * looking at it has no way to tell. An unknown *state* is not a lie — dagr's
 * vocabulary is open — so it is passed through and drawn in a neutral tone.
 */
export function parseRunDocument(value: unknown): ParseResult {
  if (!isRecord(value)) {
    return no("the service did not answer with a document");
  }
  const run = value.run;
  if (!isRecord(run) || typeof run.id !== "string" || run.id === "") {
    return no("the document has no run identity");
  }
  const rawTasks = value.tasks;
  if (!Array.isArray(rawTasks)) {
    return no("the document has no task list");
  }
  const tasks: RunTask[] = [];
  for (const [index, raw] of rawTasks.entries()) {
    const task = readTask(raw, index);
    if (typeof task === "string") {
      return no(task);
    }
    tasks.push(task);
  }
  return {
    ok: true,
    document: {
      dagr: typeof value.dagr === "number" ? value.dagr : 3,
      run: {
        id: run.id,
        title: typeof run.title === "string" ? run.title : run.id,
        started_at: text(run.started_at),
      },
      generated_at: text(value.generated_at),
      projects: readProjects(value.projects),
      tasks,
      events: readEvents(value.events),
    },
  };
}

/** The tone a state is drawn in: the mods' three colours and their neutral. */
export type Tone = "neutral" | "amber" | "green" | "red" | "accent";

/**
 * The word and tone a task state is drawn with.
 *
 * The vocabulary is the document's, not the board's: the board's eight words
 * are derived from shprd's ledger (`derived.status`) and the graph follows
 * dagr's five — a graph that reused the board's words would claim a finer
 * answer than the document carries. `review` is drawn green because the work is
 * finished and waiting on a person, which is the board's *Ready*.
 */
export function taskTone(state: string): { word: string; tone: Tone } {
  switch (state) {
    case "queued":
      return { word: "Queued", tone: "neutral" };
    case "working":
      return { word: "Working", tone: "amber" };
    case "review":
      return { word: "Review", tone: "green" };
    case "blocked":
      return { word: "Blocked", tone: "red" };
    case "done":
      return { word: "Done", tone: "neutral" };
    case "failed":
      return { word: "Failed", tone: "red" };
    case "rejected":
      return { word: "Rejected", tone: "red" };
    case "canceled":
      return { word: "Canceled", tone: "neutral" };
    case "settled_unverified":
      return { word: "Unverified", tone: "amber" };
    default:
      // A word this build has never heard of: drawn, never hidden, and never
      // guessed at.
      return { word: state === "" ? "Unknown" : state, tone: "neutral" };
  }
}

/**
 * Whether the document is saying a task was dropped. dagr's states have no word
 * for work that will not happen, so the projector writes a dropped task as
 * `blocked`, owned by the human, with a note that starts `dropped`.
 */
export function isDropped(task: RunTask): boolean {
  return (
    task.state === "blocked" &&
    task.unblock === "human" &&
    /^dropped(:|$)/.test(task.note ?? "")
  );
}

/** [`taskTone`] for a task, which also knows a dropped task is not blocked. */
export function runTaskTone(task: RunTask): { word: string; tone: Tone } {
  return isDropped(task) ? { word: "Dropped", tone: "neutral" } : taskTone(task.state);
}

/** The glyph and word for an evidence tier: the contract's ◆ ◇ ≈ ! ladder. */
export function evidenceMark(evidence: string): { glyph: string; word: string; tone: Tone } {
  switch (evidence) {
    case "verified":
      return { glyph: "◆", word: "verified", tone: "green" };
    case "reported":
      return { glyph: "◇", word: "reported", tone: "accent" };
    case "heuristic":
      return { glyph: "≈", word: "heuristic", tone: "amber" };
    case "asserted":
      return { glyph: "!", word: "asserted", tone: "amber" };
    default:
      return { glyph: "?", word: evidence === "" ? "no evidence" : evidence, tone: "neutral" };
  }
}

/** The state the latest attempt is drawn with, or absent for no attempts. */
export function latestAttempt(task: RunTask): RunAttempt | undefined {
  return task.attempts[task.attempts.length - 1];
}

/** A task's criteria, split into its sentences; dagr joins them with `"; "`. */
export function criteria(task: RunTask): string[] {
  return task.criteria
    .split("; ")
    .map((part) => part.trim())
    .filter((part) => part !== "");
}

/** One task, or why it is not one. */
function readTask(raw: unknown, index: number): RunTask | string {
  if (!isRecord(raw)) {
    return `task ${index + 1} is not an object`;
  }
  if (typeof raw.id !== "string" || raw.id === "") {
    return `task ${index + 1} has no id`;
  }
  if (typeof raw.title !== "string") {
    return `task ${raw.id} has no title`;
  }
  if (typeof raw.state !== "string" || raw.state === "") {
    return `task ${raw.id} has no state`;
  }
  return {
    id: raw.id,
    title: raw.title,
    kind: typeof raw.kind === "string" ? raw.kind : "impl",
    ...(typeof raw.project === "string" ? { project: raw.project } : {}),
    state: raw.state,
    deps: strings(raw.deps),
    criteria: typeof raw.criteria === "string" ? raw.criteria : "",
    ...(typeof raw.owner === "string" ? { owner: raw.owner } : {}),
    ...(typeof raw.unblock === "string" ? { unblock: raw.unblock } : {}),
    ...(typeof raw.note === "string" ? { note: raw.note } : {}),
    attempts: readAttempts(raw.attempts),
  };
}

/** A task's attempts, keeping only the fields a drawing reads. */
function readAttempts(raw: unknown): RunAttempt[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const attempts: RunAttempt[] = [];
  for (const entry of raw) {
    if (!isRecord(entry) || typeof entry.id !== "string") {
      continue;
    }
    const outcome = isRecord(entry.outcome)
      ? {
          result: text(entry.outcome.result),
          evidence: text(entry.outcome.evidence) as RunEvidence,
        }
      : undefined;
    attempts.push({
      id: entry.id,
      n: typeof entry.n === "number" ? entry.n : attempts.length + 1,
      cause: { type: isRecord(entry.cause) ? text(entry.cause.type) : "initial" },
      actor: text(entry.actor),
      ...(typeof entry.model === "string" ? { model: entry.model } : {}),
      state: text(entry.state),
      started_at: text(entry.started_at),
      ...(typeof entry.ended_at === "string" ? { ended_at: entry.ended_at } : {}),
      ...(outcome === undefined ? {} : { outcome }),
    });
  }
  // A document that numbered its attempts oddly still draws them in order: a
  // refusal here would hide the task over a field no screen shows.
  return attempts.sort((left, right) => left.n - right.n);
}

/** The document's project list, keeping only well-formed entries. */
function readProjects(raw: unknown): RunProject[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw.filter(isRecord).flatMap((entry) => {
    const id = typeof entry.id === "string" ? entry.id : "";
    if (id === "") {
      return [];
    }
    return [{ id, title: typeof entry.title === "string" ? entry.title : id }];
  });
}

/** The document's event tail, keeping only well-formed entries. */
function readEvents(raw: unknown): RunEvent[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw.filter(isRecord).flatMap((entry) => {
    if (typeof entry.at !== "string" || typeof entry.type !== "string") {
      return [];
    }
    return [
      {
        at: entry.at,
        type: entry.type,
        ...(typeof entry.task === "string" ? { task: entry.task } : {}),
        ...(typeof entry.attempt === "string" ? { attempt: entry.attempt } : {}),
        detail: text(entry.detail),
      },
    ];
  });
}

/** Whether a value is a JSON object. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A string field, or `""`. */
function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** An array of strings, ignoring anything that is not one. */
function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((part): part is string => typeof part === "string") : [];
}

/** A failed read. */
function no(problem: string): ParseResult {
  return { ok: false, problem };
}
