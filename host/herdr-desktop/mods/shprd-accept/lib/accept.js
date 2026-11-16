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

import { attempt, cut, taskById } from "./board.js";

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

/** `3 files changed · +12 −4`, the line above the list. */
export function changesLine(changes) {
  const files = (changes?.files ?? []).length;
  if (files === 0) return WORDS.noChanges;
  const plural = files === 1 ? "file" : "files";
  return `${files} ${plural} changed · +${changes.added ?? 0} −${changes.deleted ?? 0}`;
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
