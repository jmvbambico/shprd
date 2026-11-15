// The task PR card's reading: the pull request a task landed through, or the
// quiet line that says there was none.
//
// shprd lands every task through its own pull request when the repository has a
// forge, and merges locally when it has none; the log records both (a
// `task_pr_opened` carries the number and the URL, a local landing carries
// `number: null`). `GET /api/state` hands each task its pull request, so this
// card needs no call of its own — unlike Shepherdr's, which asks `gh` every 30
// seconds, because its task records hold only what the CLI wrote.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/task-pr/main.js` (the state word in a soft tint, the number, the
//   size of the change, the link, and a quiet line when there is nothing)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/taskpr.js`; `bin/sync-shared` copies it into
// every mod folder that uses it. Edit the source, never a copy.

/** The card's words, in one place so the card and its tests agree. */
export const PR_WORDS = {
  title: "Pull request",
  none: "No pull request yet: it opens when the task lands.",
  local: "Landed locally: this run has no forge, so its work went straight into the base branch.",
  open: "Open",
  merged: "Merged",
  approved: "Approved",
  notApproved: "Waiting for an approval",
  openOnTheForge: "Open on the forge",
};

/**
 * What the card says about `task`, and what it draws.
 *
 * Three answers, and the difference between them is the point:
 *
 * - `kind: "pr"` — the task has a pull request; `state` is `Open` or `Merged`,
 *   `tone` the colour word it is drawn in, `number` the forge's number as a
 *   person writes it and `url` the page to open.
 * - `kind: "local"` — the task landed with no forge. That is not a missing pull
 *   request to chase: there will never be one, and the card says so once.
 * - `kind: "none"` — nothing has landed yet, so no pull request exists; the card
 *   says when one would.
 */
export function taskPr(task) {
  const pr = task?.pr;
  if (pr) {
    const merged = pr.state === "merged";
    return {
      kind: "pr",
      state: merged ? PR_WORDS.merged : PR_WORDS.open,
      tone: merged ? "green" : "amber",
      number: `#${pr.number}`,
      url: pr.url ?? "",
      head: pr.head ?? "",
      approved: Boolean(pr.approved),
      approval: pr.approved ? PR_WORDS.approved : PR_WORDS.notApproved,
      // The one line a card shows under the number: what it proposes to merge
      // and whether anything approved it.
      line: pr.approved ? PR_WORDS.approved : PR_WORDS.notApproved,
    };
  }
  if (task?.status === "integrated") {
    return { kind: "local", state: PR_WORDS.merged, tone: "green", number: null, url: "", line: PR_WORDS.local };
  }
  if (task?.status === "dropped") {
    return { kind: "none", state: null, tone: "dim", number: null, url: "", line: "Dropped: it never lands." };
  }
  return { kind: "none", state: null, tone: "dim", number: null, url: "", line: PR_WORDS.none };
}

/**
 * Where a click on the link would go, or null when there is nothing to open.
 *
 * A URL the forge did not give is not one to open: an empty `open_url` call is a
 * person's window asked for nothing.
 */
export function openUrl(card) {
  return card?.kind === "pr" && typeof card.url === "string" && card.url.startsWith("http") ? card.url : null;
}
