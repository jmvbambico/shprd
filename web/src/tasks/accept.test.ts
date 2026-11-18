// The Accept card's reading: its words, its lines and which verbs it offers.

import { describe, expect, it } from "vitest";

import {
  asksFirst,
  canReview,
  changeCounts,
  changeRows,
  checks,
  findings,
  openUrl,
  reviewLine,
  taskPr,
  threadViews,
  verbs,
  whereNow,
  words,
} from "./accept";
import { attemptWithReport, task } from "./fixtures";

const ready = task("T1", "ready", { attempts: [attemptWithReport()] });

describe("words", () => {
  it("keeps a short summary whole and folds a long one under Details", () => {
    expect(words(ready)).toEqual({ summary: "Added the board.", details: "" });
    const long = "First sentence is here and it is fairly long. ".repeat(8);
    const folded = words(task("T", "ready", { attempts: [{ ...attemptWithReport(), report: { ...attemptWithReport().report!, summary: long } }] }));
    expect(folded.summary.endsWith(".")).toBe(true);
    expect(folded.summary.length).toBeLessThanOrEqual(160);
    expect(folded.details.length).toBeGreaterThan(160);
  });
});

describe("changes", () => {
  const changes = {
    task: "T1",
    base: "integration/m11",
    branch: "feature/x",
    merge_base: "abc",
    commit_count: 2,
    commits: [{ sha: "aaa1111", subject: "feat: x" }],
    files: [
      { path: "small.rs", added: 1, deleted: 0, binary: false },
      { path: "big.rs", added: 20, deleted: 4, binary: false },
      { path: "logo.png", added: 0, deleted: 0, binary: true },
    ],
    added: 21,
    deleted: 4,
  };

  it("counts with colourable parts, and says nothing for no files", () => {
    expect(changeCounts(changes)).toEqual({ commits: "2 commits", files: "3 files changed", added: "+21", removed: "−4" });
    expect(changeCounts({ ...changes, files: [] })).toBeNull();
    expect(changeCounts(null)).toBeNull();
  });

  it("lists the biggest files first and says a binary is binary", () => {
    const rows = changeRows(changes);
    expect(rows.rows.map((r) => r.path)).toEqual(["big.rs", "small.rs", "logo.png"]);
    expect(rows.rows[2]?.stat).toBe("binary");
    const many = { ...changes, files: Array.from({ length: 11 }, (_, i) => ({ path: `f${i}`, added: i, deleted: 0, binary: false })) };
    expect(changeRows(many).rest).toBe(3);
  });
});

describe("checks and review", () => {
  it("shows each recorded check with its state and the last line it printed", () => {
    const rows = checks(ready).rows;
    expect(rows.map((r) => r.state)).toEqual(["passed", "failed"]);
    expect(rows[0]?.detail).toBe("test result: ok. 9 passed");
  });

  it("lists findings worst first and names the reviewer", () => {
    expect(findings(ready).map((f) => f.word)).toEqual(["Major", "Minor"]);
    expect(reviewLine(ready)).toEqual({ line: "codex reviewed it: passed · another vendor", degraded: false });
    expect(reviewLine(task("T", "ready")).line).toBe("No review was asked for this attempt.");
  });

  it("says plainly when the review was not independent", () => {
    const degraded = task("T", "ready", {
      attempts: [{ ...attemptWithReport(), reviews: [{ reviewer: "claude", degraded: true }] }],
    });
    expect(reviewLine(degraded).degraded).toBe(true);
  });
});

describe("verbs", () => {
  it("offers Accept for reviewed work where tasks.merge asks", () => {
    expect(verbs(ready, "ask")).toMatchObject({ accept: true, drop: true, sendBack: false });
    expect(verbs(ready, null).accept).toBe(true);
    expect(asksFirst("auto")).toBe(false);
  });

  it("says so, and offers no Accept, where nothing waits for an approval", () => {
    const v = verbs(ready, "auto");
    expect(v.accept).toBe(false);
    expect(v.note).toBe("tasks.merge is auto: T1 lands without an approval");
  });

  it("offers Send back only for a failed attempt, and nothing for a settled task", () => {
    expect(verbs(task("F", "queued", { status: "failed" }), "ask").sendBack).toBe(true);
    expect(verbs(task("D", "done"), "ask")).toEqual({ accept: false, sendBack: false, drop: false, note: null });
    expect(whereNow(task("D", "done"))).toBe("Landed: its work is in the base branch.");
    expect(whereNow(null)).toBe("This task is no longer on the board.");
  });
});

describe("the thread", () => {
  it("shows Pull request only once the task has one, and dims Terminal with no agent", () => {
    expect(threadViews(ready, false, "chat").map((s) => s.key)).toEqual(["chat", "term"]);
    expect(threadViews(ready, false, "chat")[1]?.dead).toBe(true);
    const pr = task("P", "ready", { pr: { number: 7, url: "https://x/7", head: "h", state: "open", approved: true } });
    expect(threadViews(pr, true, "pr").map((s) => s.key)).toEqual(["chat", "term", "pr"]);
    expect(canReview(ready)).toBe(true);
    expect(canReview(task("W", "working"))).toBe(false);
  });

  it("says a local landing has no pull request, once", () => {
    expect(taskPr(task("L", "done")).kind).toBe("local");
    expect(taskPr(task("Q", "queued")).kind).toBe("none");
    const merged = taskPr(task("P", "done", { pr: { number: 7, url: "https://x/7", head: "h", state: "merged", approved: true } }));
    expect(merged).toMatchObject({ kind: "pr", state: "Merged", number: "#7" });
    expect(openUrl(merged)).toBe("https://x/7");
    expect(openUrl({ ...merged, url: "javascript:alert(1)" })).toBeNull();
  });
});
