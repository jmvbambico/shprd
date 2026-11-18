// The Accept card's reading: what a person is approving, as lines a card draws.
//
// Everything here is a pure function of what the service answered — `GET
// /api/state` for the task, `GET /api/tasks/<id>/changes` for its branch, `GET
// /api/settings` for the merge policy — so a card, a test and a screenshot
// cannot disagree about what is on it.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/accept.js` (the card's words, the summary that folds into
//   Details, the Changes count line and the list folded past a few), and
//   `mods/accept-card/main.js` (the sections: summary, Changes, Checks, a folded
//   Details, then the footer's one primary)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the changes come from the task's branch (`GET
// /api/tasks/<id>/changes`, read from the merge base), the checks are the ones
// the **hand-in** recorded rather than a forge's check runs, and the verbs are
// the engine's — Accept is `shprd approve --task`, Send back is the same spawn a
// failed review's send-back uses, Drop is `shprd drop` — with Accept shown only
// where `tasks.merge` is `ask`.
//
// Shared: the source is `mods/lib/accept.js`; `bin/sync-shared` copies it into
// every mod folder that uses it. Edit the source, never a copy.

import { attempt, cut, rowActions, taskById } from "./board.js";
import { agentsOfTask } from "./tree.js";

/** How much of an agent's summary a card shows before the rest folds away. */
export const SUMMARY_MAX = 160;

/** How many changed files a card lists before it says how many are left. */
export const FILES_SHOWN = 8;

/** The card's words, in one place so the header, the sections and the tests agree. */
export const WORDS = {
  title: "Accept",
  question: (ref) => `Accept ${ref}?`,
  reading: "Reading the task…",
  notYet: "not acceptable right now",
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
  moreLine: (n) => `And ${n} more`,
  reasonLabel: "Why drop it",
  reasonPlaceholder: "One line, kept on the pull request",
};

/** The severity words, keyed by the API's `severity`, each with the tone it is drawn in. */
export const SEVERITY = {
  critical: { word: "Critical", tone: "red", rank: 0 },
  major: { word: "Major", tone: "red", rank: 1 },
  minor: { word: "Minor", tone: "amber", rank: 2 },
  trivial: { word: "Trivial", tone: "dim", rank: 3 },
};

/** The states a check can be in, in the words a card shows. */
export const CHECK_STATE = { passed: "Passed", failed: "Failed", unknown: "No exit code" };

/** Whether `policy` leaves anything for a person to accept. */
export function asksFirst(policy) {
  // A service that could not be asked for its settings answers `null`, and then
  // the engine's own rules are the judge: showing the button lets the person try
  // and hear the refusal, which is better than hiding a verb that may work.
  return policy === null || policy === undefined || policy === "ask";
}

/**
 * The task the card is about, from a snapshot and the id the panel picked.
 *
 * A task the board no longer holds is not an error: it landed or was dropped
 * while the card was open, and the card says so rather than drawing an empty
 * shell.
 */
export function card(state, id) {
  return taskById(state, id);
}

/**
 * The agent's summary as the card reads it: the first sentence or so at the top,
 * and the whole text folded under Details when it did not fit.
 *
 * It is Shepherdr's rule (`core/accept.js`, `words()`): a summary an older build
 * wrote could be a paragraph, and the top of the card is one line for the person,
 * not a wall.
 */
export function words(task) {
  const said = String(attempt(task)?.report?.summary ?? "")
    .replace(/\s+/g, " ")
    .trim();
  if (said.length <= SUMMARY_MAX) return { summary: said, details: "" };
  const head = said.slice(0, SUMMARY_MAX);
  const stop = Math.max(head.lastIndexOf(". "), head.lastIndexOf("! "), head.lastIndexOf("? "));
  const cutAt =
    stop >= 40 ? head.slice(0, stop + 1) : `${head.slice(0, Math.max(head.lastIndexOf(" "), 40)).trim()}…`;
  return { summary: cutAt, details: said };
}

/** `3 commits`, `1 commit`. */
export function commitsLine(count) {
  return `${count} ${count === 1 ? "commit" : "commits"}`;
}

/** `4 files changed`, `1 file changed`. */
export function filesLine(count) {
  return `${count} ${count === 1 ? "file" : "files"} changed`;
}

/**
 * The line above the list, in its parts: `3 commits · 4 files changed +12 −4`.
 *
 * Shepherdr's Changes line (`mods/accept-card/main.js` `changes()`): the counts
 * are drawn in separate runs so the `+a` is green and the `−r` red. `commits` is
 * null when the service did not say (an older service has no `commit_count`), and
 * the line is then the files alone, never a made-up zero.
 */
export function changeCounts(changes) {
  const files = (changes?.files ?? []).length;
  if (files === 0) return null;
  return {
    commits: typeof changes?.commit_count === "number" ? commitsLine(changes.commit_count) : null,
    files: filesLine(files),
    added: `+${changes.added ?? 0}`,
    removed: `−${changes.deleted ?? 0}`,
  };
}

/** The Changes line as plain text; the card draws [`changeCounts`] in colour. */
export function changesLine(changes) {
  const parts = changeCounts(changes);
  if (parts === null) return WORDS.noChanges;
  return `${[parts.commits, parts.files].filter(Boolean).join(" · ")} ${parts.added} ${parts.removed}`;
}

/**
 * The newest commits the service named, and how many more there are.
 *
 * `rest` is the count past what is listed, so the card says "And 2 more" instead
 * of showing five of seven as if they were all of them.
 */
export function commitRows(changes) {
  const rows = (changes?.commits ?? []).map((commit) => ({ sha: commit.sha, subject: commit.subject }));
  const total = typeof changes?.commit_count === "number" ? changes.commit_count : rows.length;
  return { rows, rest: Math.max(0, total - rows.length) };
}

/**
 * The changed files, biggest first, at most [`FILES_SHOWN`].
 *
 * `rest` is how many were folded away, so a card says "and 4 more" rather than
 * silently showing eight of twelve. A binary file has no counts and says so,
 * instead of claiming `+0 −0`.
 */
export function changeRows(changes) {
  const files = [...(changes?.files ?? [])].sort(
    (left, right) => right.added + right.deleted - (left.added + left.deleted),
  );
  const shown = files.slice(0, FILES_SHOWN).map((file) => ({
    path: file.path,
    added: file.added ?? 0,
    deleted: file.deleted ?? 0,
    binary: Boolean(file.binary),
    stat: file.binary ? "binary" : `+${file.added ?? 0} −${file.deleted ?? 0}`,
  }));
  return { rows: shown, rest: Math.max(0, files.length - shown.length) };
}

/**
 * The checks the hand-in recorded: the gates the worker ran for it, with what
 * each said.
 *
 * It is the report's own `checks`, which is what a person can verify — the
 * command and its exit code — rather than a claim that everything passed. A
 * hand-in recorded before checks existed has `tests` strings instead, and those
 * are carried separately so the card can say what did run.
 */
export function checks(task) {
  const report = attempt(task)?.report;
  const rows = (report?.checks ?? []).map((check) => {
    const state =
      check.exit_code === 0 ? "passed" : check.exit_code === null || check.exit_code === undefined ? "unknown" : "failed";
    const tail = String(check.output_tail ?? "").trim();
    return {
      name: check.gate || check.command,
      command: check.command,
      state,
      word: CHECK_STATE[state],
      // One line of what it printed: the tail's last line is where a test
      // summary sits, and a whole tail would push the review off the card.
      detail: tail === "" ? "" : cut(tail.split("\n").filter((line) => line.trim() !== "").pop(), 80),
    };
  });
  return { rows, alsoRan: report?.tests ?? [] };
}

/**
 * The reviewer's findings, worst first, with the wave they came from.
 *
 * Every gate that reported findings contributes, so a review and a failing gate
 * of the engine's own read as one list — but a review's findings carry the
 * reviewer's name, because a person deciding is deciding about *that* reviewer's
 * objection.
 */
export function findings(task) {
  const attempt_ = attempt(task);
  const reviewers = (attempt_?.reviews ?? []).map((review) => review.reviewer);
  const rows = [];
  for (const gate of attempt_?.gates ?? []) {
    for (const finding of gate.findings ?? []) {
      const shape = SEVERITY[finding.severity] ?? { word: "Finding", tone: "dim", rank: 9 };
      rows.push({
        severity: finding.severity,
        word: shape.word,
        tone: shape.tone,
        rank: shape.rank,
        gate: gate.gate,
        reviewer: gate.gate === "review" ? (reviewers[reviewers.length - 1] ?? null) : null,
        title: finding.title,
        location: finding.location ?? null,
        detail: finding.detail ?? "",
      });
    }
  }
  return rows.sort((left, right) => left.rank - right.rank);
}

/**
 * Who reviewed the attempt, in one line: the reviewer and whether it was an
 * independent one.
 *
 * A degraded review — the reviewer sharing the author's vendor — is said plainly:
 * it is the one thing about a review a person must not have to infer.
 */
export function reviewLine(task) {
  const attempt_ = attempt(task);
  const reviews = attempt_?.reviews ?? [];
  if (reviews.length === 0) {
    const gate = (attempt_?.gates ?? []).find((entry) => entry.gate === "review");
    if (!gate) return { line: WORDS.noReview, degraded: false };
  }
  const latest = reviews[reviews.length - 1];
  const who = latest?.reviewer ?? "a reviewer";
  const verdict = (attempt_?.gates ?? []).filter((entry) => entry.gate === "review").pop();
  const passed = verdict ? (verdict.passed ? "passed" : "failed") : null;
  const vendor = latest?.degraded ? " · same vendor, not independent" : " · another vendor";
  const degraded = Boolean(latest?.degraded) || (verdict ? !verdict.proven : false);
  return {
    line: passed ? `${who} reviewed it: ${passed}${vendor}` : `${who} is reviewing it${vendor}`,
    degraded,
  };
}

/**
 * What the card's footer offers, and why.
 *
 * The verbs are the engine's: `task.accept` records the person's approval at the
 * branch's head, `task.retry` is the spawn a failed review's send-back runs, and
 * `task.drop` is the person's decision not to do the task. Accept is only
 * offered where `tasks.merge` is `ask` — with `checks` or `auto` nothing waits
 * for an approval, so a button that recorded one would be a decision with no
 * effect — and `note` says that in words rather than leaving the footer empty.
 *
 * A task that already landed, or was dropped, offers nothing: the card then says
 * where the task is instead ([`whereNow`]).
 */
export function verbs(task, policy) {
  const status = task?.status;
  const live = status !== "integrated" && status !== "dropped";
  if (!live) return { accept: false, sendBack: false, drop: false, note: null };
  // Accept is for work that has been decided: a review that passed, which is
  // what `passed` and the Ready word mean. A hand-in nobody has reviewed yet is
  // not a decision waiting for the person, and an approval there would be a
  // stamp on work the run has not looked at.
  const reviewed = status === "passed" || task?.derived?.status === "ready";
  const accept = reviewed && asksFirst(policy);
  const note = reviewed && !asksFirst(policy)
    ? `tasks.merge is ${policy}: ${task?.id ?? "it"} lands without an approval`
    : null;
  // A send-back is a spawn, and the rules refuse one while the task is not
  // failed: a "send back" on work in progress would spawn a second attempt at a
  // task that already has one.
  const sendBack = status === "failed";
  const drop = live;
  return { accept, sendBack, drop, note };
}

/** Where a task the card can no longer accept is now, calmly. */
export function whereNow(task) {
  if (!task) return "This task is no longer on the board.";
  if (task.status === "dropped") return "Dropped: the reason is on the run's pull request.";
  if (task.status === "integrated") return "Landed: its work is in the base branch.";
  return `Not ready right now: it is ${task?.derived?.status ?? "elsewhere"}.`;
}

/**
 * The card's header: the task's reference, its title and the word for it.
 *
 * It is the same shape as a board row so the drill-in reads as the row grown
 * bigger; the caller draws the glyph.
 */
export function head(task) {
  if (!task) return { id: null, title: "", word: "", tone: "dim", question: WORDS.title };
  const derived = task.derived ?? {};
  return {
    id: task.id,
    title: task.title ?? "",
    word: (derived.status ?? "").replace(/_/g, " "),
    tone: derived.status === "ready" ? "green" : derived.status === "needs_you" ? "red" : "dim",
    question: WORDS.question(task.id),
  };
}

/** The thread's views, by the stack child each one shows. */
export const VIEW = {
  chat: { id: "tasks-thread-chat", word: "Chat" },
  term: { id: "tasks-thread-term", word: "Terminal" },
  // The Terminal view's fallback: the agent's screen as read-only text, for an
  // agent whose own terminal the window cannot show.
  stream: { id: "tasks-thread-stream", word: "Terminal" },
  pr: { id: "tasks-thread-pr", word: "Pull request" },
  artifacts: { id: "tasks-thread-artifacts", word: "Artifacts" },
};

/**
 * The agent a task's Terminal view is about: the newest agent of the task that is
 * still running, else the newest that ended (its screen stays readable), else null.
 *
 * A task has a worker and, once it handed in, a reviewer; the person who opens
 * Terminal wants whoever is at work now, which is the newer of the two.
 */
export function threadAgent(state, taskId, now = Date.now()) {
  const mine = agentsOfTask(state, taskId, now);
  const newest = (list) =>
    list.reduce((best, agent) => ((agent.started_ms ?? 0) >= (best?.started_ms ?? -1) ? agent : best), null);
  return newest(mine.filter((agent) => agent.state !== "done")) ?? newest(mine);
}

/**
 * Whether the window holds `agent`'s terminal, so its block can show it: the
 * terminal list the window reads (`terminals()`) names it by `address`, and a
 * finished program is not a terminal to type into.
 */
export function terminalReachable(agent, terminals) {
  if (!agent?.terminal || agent.state === "done") return false;
  return (terminals ?? []).some(
    (entry) => entry?.address === agent.terminal && !["done", "exited", "finished"].includes(String(entry?.state ?? "")),
  );
}

/**
 * The stack child the Terminal word shows: the agent's own live terminal when the
 * window holds it ([`VIEW.term`]), else its read-only screen ([`VIEW.stream`]).
 */
export function terminalChild(agent, terminals) {
  return terminalReachable(agent, terminals) ? VIEW.term.id : VIEW.stream.id;
}

/**
 * Whether a task has anything for the Artifacts view: a hand-in report, which is
 * what a worker leaves behind (the summary, its checks, the files it changed and
 * the proof it names).
 */
export function hasArtifacts(task) {
  return (task?.attempts ?? []).some((attempt) => attempt?.report);
}

/**
 * The segmented switch the thread head draws: `Chat · Terminal · Pull request ·
 * Artifacts`.
 *
 * Shepherdr's switcher (`mods/chat/panel.js`, `segs`): Pull request only once the
 * task has one (`hasPr`), Terminal dim when the task has no agent at all (`live`
 * false: "Not running"), Artifacts once there is a hand-in, and no Files or
 * Changes — a task's changes are its pull request's. Chat is always on: it reads
 * the agent's record through the service, and says so when there is none yet.
 * `shown` is the stack child on screen, and Chat is what a thread opens on.
 *
 * `termChild` is the child the Terminal word shows ([`terminalChild`]); the
 * Terminal segment is lit for either of its two children.
 *
 * Each segment: `{ key, word, on, dead, child }`, `child` the stack child it shows.
 */
export function threadViews(task, { live = false, shown = VIEW.chat.id, termChild = VIEW.term.id } = {}) {
  const segs = [
    { key: "chat", word: VIEW.chat.word, on: shown === VIEW.chat.id, dead: false, child: VIEW.chat.id },
    {
      key: "term",
      word: VIEW.term.word,
      on: shown === VIEW.term.id || shown === VIEW.stream.id,
      dead: !live,
      child: termChild,
    },
  ];
  if (task?.pr) {
    segs.push({ key: "pr", word: VIEW.pr.word, on: shown === VIEW.pr.id, dead: false, child: VIEW.pr.id });
  }
  if (hasArtifacts(task)) {
    segs.push({ key: "artifacts", word: VIEW.artifacts.word, on: shown === VIEW.artifacts.id, dead: false, child: VIEW.artifacts.id });
  }
  return segs;
}

/**
 * Whether the thread must fall back to the terminal: it shows the pull request of
 * a task that has none (Shepherdr's `chat/panel.js:367`, "back to its chat": the view it opens on).
 */
export function mustLeavePr(task, shown) {
  return (shown === VIEW.pr.id && !task?.pr) || (shown === VIEW.artifacts.id && !hasArtifacts(task));
}

/**
 * The agent's last status line under the title, `message` and how far it said it
 * had got (`· 40%`), or null when it said nothing.
 *
 * Shepherdr's `reportLine` ends with minutes left; the service reports none, so
 * this ends with the progress percent it does report.
 */
export function reportLine(task) {
  const said = String(task?.last_status?.message ?? "").trim();
  if (said === "") return null;
  const done = task?.last_status?.progress;
  return typeof done === "number" ? `${said} · ${done}%` : said;
}

/**
 * Whether the task head offers Review: the same rule as the Ready row's pill
 * (`rowActions`), so the two never disagree.
 */
export function canReview(task) {
  return rowActions(task).some((act) => act.id === "review");
}
