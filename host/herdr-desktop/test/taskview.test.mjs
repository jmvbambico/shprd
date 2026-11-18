// The thread's read-only stream and Artifacts view: pure readings of what the
// service answered (`mods/lib/taskview.js`).

import test from "node:test";
import assert from "node:assert/strict";

import { STREAM_LINES, artifactSections, streamLines } from "../mods/lib/taskview.js";

test("a screen becomes lines: trailing blanks dropped, tabs widened, only the tail kept", () => {
  assert.deepEqual(streamLines("a\r\nb\tc\n\n  \n"), ["a", "b    c"]);
  assert.deepEqual(streamLines(null), [""].slice(1));
  const long = Array.from({ length: STREAM_LINES + 30 }, (_, n) => `l${n}`).join("\n");
  const lines = streamLines(long);
  assert.equal(lines.length, STREAM_LINES);
  assert.equal(lines.at(-1), `l${STREAM_LINES + 29}`);
});

test("artifacts are the hand-ins and reviews, newest attempt first", () => {
  const task = {
    attempts: [
      { n: 1, agent: "claude", report: { status: "failed", summary: "first", checks: [], tests: [], changed_paths: [] }, reviews: [] },
      {
        n: 2,
        agent: "claude",
        report: {
          status: "done",
          summary: "second try",
          checks: [
            { gate: "tests", command: "cargo test", exit_code: 0 },
            { gate: "lint", command: "cargo clippy", exit_code: 101 },
          ],
          tests: ["unit"],
          changed_paths: ["a.rs", "b.rs"],
          commit: "0123456789abcdef",
        },
        reviews: [{ reviewer: "codex", degraded: false }],
      },
      { n: 3, agent: "claude", reviews: [] },
    ],
  };
  const sections = artifactSections(task);
  assert.deepEqual(
    sections.map((section) => section.title),
    ["Attempt 2 · claude · done", "Attempt 1 · claude · failed"],
    "an attempt that never handed in has no section",
  );
  const rows = Object.fromEntries(sections[0].rows.map((row) => [row.key, row]));
  assert.equal(rows.tests.tone, "green");
  assert.equal(rows.lint.tone, "red");
  assert.equal(rows["Changed (2)"].value, "a.rs\nb.rs");
  assert.equal(rows.Commit.value, "0123456789");
  assert.equal(rows.Review.value, "codex");
  assert.deepEqual(artifactSections({ attempts: [] }), []);
  assert.deepEqual(artifactSections(null), []);
});
