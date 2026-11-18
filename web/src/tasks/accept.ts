// The Accept card's reading: what a person is approving, as lines a card draws.
//
// Everything here is a pure function of what the service answered — the board
// for the task, `GET /api/tasks/{id}/changes` for its branch, `GET /api/settings`
// for the merge policy — and it is the web's copy of the Herdr mods'
// `lib/accept.js` and `lib/taskpr.js`, so the window and the phone say the same
// words about the same task.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/accept.js` (the card's words, the summary that folds into
//   Details, the Changes count line), `mods/accept-card/main.js` (the sections),
//   `mods/task-pr/main.js` (the pull-request view)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { TaskChangesDto, TaskDto } from "../gen";
import { attempt, cut, rowActions } from "./board";

/** How much of an agent's summary a card shows before the rest folds away. */
export const SUMMARY_MAX = 160;

/** How many changed files a card lists before it says how many are left. */
export const FILES_SHOWN = 8;

/** The card's words, in one place. */
export const WORDS = {
  title: "Accept",
  question: (ref: string) => `Accept ${ref}?`,
  reading: "Reading the task…",
  noSummary: "The agent wrote no summary.",
  changes: "Changes",
  checks: "Checks",
  review: "Review",
  details: "Details",
  noChanges: "Nothing changed on its branch yet.",
  noChecks: "The hand-in named no checks.",
  noReview: "No review was asked for this attempt.",
  accept: "Accept",
  accepting: "Accepting…",
  sendBack: "Send back",
  sendingBack: "Sending back…",
  drop: "Drop task",
  dropping: "Dropping…",
  cancel: "Cancel",
  close: "Close",
  moreLine: (n: number) => `And ${n} more`,
  reasonPlaceholder: "One line, kept on the pull request",
} as const;

/** The severity words, each with the tone it is drawn in. */
export const SEVERITY: Record<string, { word: string; tone: "red" | "amber" | "dim"; rank: number }> = {
  critical: { word: "Critical", tone: "red", rank: 0 },
  major: { word: "Major", tone: "red", rank: 1 },
  minor: { word: "Minor", tone: "amber", rank: 2 },
  trivial: { word: "Trivial", tone: "dim", rank: 3 },
};

/** Whether the merge policy leaves anything for a person to accept (unknown: yes, the engine decides). */
export function asksFirst(policy: string | null | undefined): boolean {
  return policy === null || policy === undefined || policy === "ask";
}

/** The agent's summary: the first sentence or so on top, the whole text under Details. */
export function words(task: TaskDto | null): { summary: string; details: string } {
  const said = String(attempt(task)?.report?.summary ?? "")
    .replace(/\s+/g, " ")
    .trim();
  if (said.length <= SUMMARY_MAX) {
    return { summary: said, details: "" };
  }
  const head = said.slice(0, SUMMARY_MAX);
  const stop = Math.max(head.lastIndexOf(". "), head.lastIndexOf("! "), head.lastIndexOf("? "));
  const head2 =
    stop >= 40 ? head.slice(0, stop + 1) : `${head.slice(0, Math.max(head.lastIndexOf(" "), 40)).trim()}…`;
  return { summary: head2, details: said };
}

/** `3 commits`, `1 commit`. */
export function commitsLine(count: number): string {
  return `${count} ${count === 1 ? "commit" : "commits"}`;
}

/** `4 files changed`, `1 file changed`. */
export function filesLine(count: number): string {
  return `${count} ${count === 1 ? "file" : "files"} changed`;
}

/** The Changes line in its parts, so `+a` can be green and `−r` red; null with no files. */
export function changeCounts(
  changes: TaskChangesDto | null,
): { commits: string | null; files: string; added: string; removed: string } | null {
  const files = (changes?.files ?? []).length;
  if (changes === null || files === 0) {
    return null;
  }
  return {
    commits: typeof changes.commit_count === "number" ? commitsLine(changes.commit_count) : null,
    files: filesLine(files),
    added: `+${changes.added ?? 0}`,
    removed: `−${changes.deleted ?? 0}`,
  };
}

/** The newest commits the service named, and how many more there are. */
export function commitRows(changes: TaskChangesDto | null): {
  rows: { sha: string; subject: string }[];
  rest: number;
} {
  const rows = (changes?.commits ?? []).map((c) => ({ sha: c.sha, subject: c.subject }));
  const total = typeof changes?.commit_count === "number" ? changes.commit_count : rows.length;
  return { rows, rest: Math.max(0, total - rows.length) };
}

/** The changed files, biggest first, at most [`FILES_SHOWN`], and how many were folded away. */
export function changeRows(changes: TaskChangesDto | null): {
  rows: { path: string; stat: string; binary: boolean }[];
  rest: number;
} {
  const files = [...(changes?.files ?? [])].sort(
    (a, b) => b.added + b.deleted - (a.added + a.deleted),
  );
  const rows = files.slice(0, FILES_SHOWN).map((f) => ({
    path: f.path,
    binary: Boolean(f.binary),
    stat: f.binary ? "binary" : `+${f.added ?? 0} −${f.deleted ?? 0}`,
  }));
  return { rows, rest: Math.max(0, files.length - rows.length) };
}

/** One check row. */
export interface CheckRow {
  name: string;
  command: string;
  state: "passed" | "failed" | "unknown";
  word: string;
  detail: string;
}

/** The checks the hand-in recorded, with what each said. */
export function checks(task: TaskDto | null): { rows: CheckRow[]; alsoRan: string[] } {
  const report = attempt(task)?.report;
  const rows = (report?.checks ?? []).map((check): CheckRow => {
    const state =
      check.exit_code === 0
        ? "passed"
        : check.exit_code === null || check.exit_code === undefined
          ? "unknown"
          : "failed";
    const lines = String(check.output_tail ?? "")
      .trim()
      .split("\n")
      .filter((line) => line.trim() !== "");
    return {
      name: check.gate || check.command,
      command: check.command,
      state,
      word: { passed: "Passed", failed: "Failed", unknown: "No exit code" }[state],
      detail: lines.length === 0 ? "" : cut(lines[lines.length - 1], 80),
    };
  });
  return { rows, alsoRan: report?.tests ?? [] };
}

/** One finding row. */
export interface FindingRow {
  severity: string;
  word: string;
  tone: "red" | "amber" | "dim";
  rank: number;
  gate: string;
  title: string;
  location: string | null;
  detail: string;
}

/** The findings of the newest attempt, worst first. */
export function findings(task: TaskDto | null): FindingRow[] {
  const latest = attempt(task);
  const rows: FindingRow[] = [];
  for (const gate of latest?.gates ?? []) {
    for (const finding of gate.findings ?? []) {
      const shape = SEVERITY[finding.severity] ?? { word: "Finding", tone: "dim" as const, rank: 9 };
      rows.push({
        severity: finding.severity,
        word: shape.word,
        tone: shape.tone,
        rank: shape.rank,
        gate: gate.gate,
        title: finding.title,
        location: finding.location ?? null,
        detail: finding.detail ?? "",
      });
    }
  }
  return rows.sort((a, b) => a.rank - b.rank);
}

/** Who reviewed the attempt, in one line; a same-vendor review says so plainly. */
export function reviewLine(task: TaskDto | null): { line: string; degraded: boolean } {
  const latest = attempt(task);
  const reviews = latest?.reviews ?? [];
  const verdict = (latest?.gates ?? []).filter((g) => g.gate === "review").pop();
  if (reviews.length === 0 && !verdict) {
    return { line: WORDS.noReview, degraded: false };
  }
  const last = reviews[reviews.length - 1];
  const who = last?.reviewer ?? "a reviewer";
  const passed = verdict ? (verdict.passed ? "passed" : "failed") : null;
  const vendor = last?.degraded ? " · same vendor, not independent" : " · another vendor";
  const degraded = Boolean(last?.degraded) || (verdict ? !verdict.proven : false);
  return {
    line: passed ? `${who} reviewed it: ${passed}${vendor}` : `${who} is reviewing it${vendor}`,
    degraded,
  };
}

/** What the card's footer offers. */
export interface Verbs {
  accept: boolean;
  sendBack: boolean;
  drop: boolean;
  note: string | null;
}

/**
 * The verbs the card's footer offers: Accept where `tasks.merge` is `ask` and the
 * work was reviewed, Send back for a failed attempt, Drop for any live task.
 */
export function verbs(task: TaskDto | null, policy: string | null): Verbs {
  const status = task?.status;
  const live = status !== "integrated" && status !== "dropped";
  if (!task || !live) {
    return { accept: false, sendBack: false, drop: false, note: null };
  }
  const reviewed = status === "passed" || task.derived.status === "ready";
  return {
    accept: reviewed && asksFirst(policy),
    sendBack: status === "failed",
    drop: live,
    note:
      reviewed && !asksFirst(policy)
        ? `tasks.merge is ${policy}: ${task.id} lands without an approval`
        : null,
  };
}

/** Where a task the card can no longer accept is now, calmly. */
export function whereNow(task: TaskDto | null): string {
  if (!task) {
    return "This task is no longer on the board.";
  }
  if (task.status === "dropped") {
    return "Dropped: the reason is on the run's pull request.";
  }
  if (task.status === "integrated") {
    return "Landed: its work is in the base branch.";
  }
  return `Not ready right now: it is ${task.derived.status ?? "elsewhere"}.`;
}

/** The thread's three views. */
export type ThreadView = "chat" | "term" | "pr";

/** One segment of the thread head's switch. */
export interface Seg {
  key: ThreadView;
  word: string;
  on: boolean;
  dead: boolean;
}

/**
 * The switch the thread head draws: `Chat · Terminal · Pull request`. Pull
 * request only once the task has one, Terminal dim when no agent is up.
 */
export function threadViews(task: TaskDto | null, live: boolean, shown: ThreadView): Seg[] {
  const segs: Seg[] = [
    { key: "chat", word: "Chat", on: shown === "chat", dead: false },
    { key: "term", word: "Terminal", on: shown === "term", dead: !live },
  ];
  if (task?.pr) {
    segs.push({ key: "pr", word: "Pull request", on: shown === "pr", dead: false });
  }
  return segs;
}

/** The agent's last status line under the title, with how far it said it had got. */
export function reportLine(task: TaskDto | null): string | null {
  const said = String(task?.last_status?.message ?? "").trim();
  if (said === "") {
    return null;
  }
  const done = task?.last_status?.progress;
  return typeof done === "number" ? `${said} · ${done}%` : said;
}

/** Whether the head offers Review: the same rule as the Ready row's pill. */
export function canReview(task: TaskDto | null): boolean {
  return rowActions(task).some((act) => act.id === "review");
}

/** The pull-request view's words. */
export const PR_WORDS = {
  title: "Pull request",
  none: "No pull request yet: it opens when the task lands.",
  local: "Landed locally: this run has no forge, so its work went straight into the base branch.",
  approved: "Approved",
  notApproved: "Waiting for an approval",
  openOnGitHub: "Open on GitHub",
  copyLink: "Copy link",
  copied: "Copied",
} as const;

/** What the pull-request view says about a task. */
export interface PrCard {
  kind: "pr" | "local" | "none";
  state: string | null;
  tone: "green" | "amber" | "dim";
  number: string | null;
  url: string;
  head: string;
  line: string;
}

/** The pull request, a local landing (no forge: say so once) or nothing yet. */
export function taskPr(task: TaskDto | null): PrCard {
  const pr = task?.pr;
  if (pr) {
    const merged = pr.state === "merged";
    return {
      kind: "pr",
      state: merged ? "Merged" : "Open",
      tone: merged ? "green" : "amber",
      number: `#${pr.number}`,
      url: pr.url ?? "",
      head: pr.head ?? "",
      line: pr.approved ? PR_WORDS.approved : PR_WORDS.notApproved,
    };
  }
  if (task?.status === "integrated") {
    return { kind: "local", state: "Merged", tone: "green", number: null, url: "", head: "", line: PR_WORDS.local };
  }
  if (task?.status === "dropped") {
    return { kind: "none", state: null, tone: "dim", number: null, url: "", head: "", line: "Dropped: it never lands." };
  }
  return { kind: "none", state: null, tone: "dim", number: null, url: "", head: "", line: PR_WORDS.none };
}

/** Where the link goes, or null when it is not a web page. */
export function openUrl(card: PrCard): string | null {
  return card.kind === "pr" && /^https?:\/\//i.test(card.url) ? card.url : null;
}

/** `4 files +12 −3`, or null before the changes were read. */
export function prSize(changes: TaskChangesDto | null): { files: string; added: string; removed: string } | null {
  const files = (changes?.files ?? []).length;
  if (changes === null || files === 0) {
    return null;
  }
  return {
    files: `${files} ${files === 1 ? "file" : "files"}`,
    added: `+${changes.added ?? 0}`,
    removed: `−${changes.deleted ?? 0}`,
  };
}
