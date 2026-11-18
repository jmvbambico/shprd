// What a task's thread shows when it is not the chat, the live terminal or the
// pull request: the agent's screen as read-only text (the Terminal view's
// fallback) and the task's artifacts, which are what its agents left behind.
//
// Everything here is a pure function of what the service answered, so the view,
// a test and a screenshot cannot disagree. Shepherdr's thread has an Artifacts
// entry for the pages its agents publish; shprd's agents publish none, so what a
// task has here is its hand-ins and reviews, read from the run's log.
//
// Shared: the source is `mods/lib/taskview.js`; `bin/sync-shared` copies it into
// every mod folder that uses it. Edit the source, never a copy.

/** The words the two views use, in one place so the view and the tests agree. */
export const VIEW_WORDS = {
  streamTitle: "Terminal",
  streamFallback: "Read-only: this agent's terminal cannot be shown here.",
  noAgent: "No agent is running for this task.",
  noOutput: "Nothing on its screen yet.",
  artifactsTitle: "Artifacts",
  noArtifacts: "Nothing handed in yet.",
};

/** How many lines of an agent's screen the stream keeps. */
export const STREAM_LINES = 200;

/**
 * An agent's screen as lines for the stream: trailing blanks dropped, the last
 * [`STREAM_LINES`] kept, tabs widened so a column stays a column.
 */
export function streamLines(text) {
  const lines = String(text ?? "")
    .replace(/\r/g, "")
    .replace(/\t/g, "    ")
    .split("\n");
  while (lines.length > 0 && lines[lines.length - 1].trim() === "") lines.pop();
  return lines.slice(-STREAM_LINES);
}

/**
 * The thread's artifacts, newest attempt first: for each attempt that handed in,
 * the summary the agent wrote, the checks it ran with their result, the files it
 * changed, its commit and the reviewers it was given.
 *
 * Each section: `{ title, rows }`, a row `{ key, value, tone }` where `tone` is
 * `green`, `red` or `dim`.
 */
export function artifactSections(task) {
  const sections = [];
  for (const attempt of [...(task?.attempts ?? [])].reverse()) {
    const report = attempt?.report;
    if (!report) continue;
    const rows = [];
    if (report.summary) rows.push({ key: "Summary", value: report.summary, tone: "dim" });
    for (const check of report.checks ?? []) {
      const ok = check.exit_code === 0;
      rows.push({
        key: check.gate ?? "check",
        value: `${check.command ?? ""}${ok ? " · passed" : " · failed"}`.trim(),
        tone: ok ? "green" : "red",
      });
    }
    for (const test of report.tests ?? []) rows.push({ key: "Test", value: String(test), tone: "dim" });
    const files = report.changed_paths ?? [];
    if (files.length > 0) rows.push({ key: `Changed (${files.length})`, value: files.join("\n"), tone: "dim" });
    if (report.commit) rows.push({ key: "Commit", value: String(report.commit).slice(0, 10), tone: "dim" });
    for (const review of attempt.reviews ?? []) {
      rows.push({
        key: "Review",
        value: `${review.reviewer}${review.degraded ? " · same vendor" : ""}`,
        tone: review.degraded ? "red" : "dim",
      });
    }
    sections.push({ title: `Attempt ${attempt.n} · ${attempt.agent ?? ""}${report.status ? ` · ${report.status}` : ""}`, rows });
  }
  return sections;
}
