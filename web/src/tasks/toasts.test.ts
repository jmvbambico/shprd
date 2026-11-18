// Toasts: which events are news, how they are worded, and that one event is one
// toast however many times it is read.

import { describe, expect, it } from "vitest";

import { attemptWithReport, snapshot, task } from "./fixtures";
import { KEEP_MS, SHOWN, clip, dismiss, fromSnapshot, mergeNews, newsItems, passItem, visible, timeoutOf } from "./toasts";

const failedAttempt = (() => {
  const a = attemptWithReport();
  a.gates = [{ gate: "review", passed: false, proven: true, findings: [{ severity: "major", title: "Loses a write", detail: "" }] }];
  return a;
})();

describe("newsItems", () => {
  const state = snapshot(
    [
      task("T1", "done", { title: "Land me" }),
      task("T2", "needs_you", { derived: { status: "needs_you", reason: "Wants a yes" } }),
      task("T3", "queued", { attempts: [failedAttempt] }),
    ],
    { needs: [{ task: "T2", body: "Pick a branch", default_action: "drop", seq: 3 }] },
  );

  it("tells a landing, a need and a failed review in the log's words", () => {
    const items = newsItems(
      [
        { seq: 5, at_ms: 1, kind: "task_landed", task: "T1" },
        { seq: 6, at_ms: 2, kind: "ask_raised", task: "T2" },
        { seq: 7, at_ms: 3, kind: "gate_failed", task: "T3" },
        { seq: 8, at_ms: 4, kind: "something_else", task: "T3" },
      ],
      state,
    );
    expect(items.map((i) => [i.id, i.level, i.title])).toEqual([
      ["ev:5", "success", "T1 · Landed"],
      ["ev:6", "warning", "T2 · Needs you"],
      ["ev:7", "error", "T3 · Review failed"],
    ]);
    expect(items[0]?.body).toBe("Land me · its work is on integration/m11");
    expect(items[1]?.body).toBe("Pick a branch");
    expect(items[2]?.body).toBe("Loses a write");
  });

  it("does not tell a need for a task that is over", () => {
    expect(newsItems([{ seq: 1, at_ms: 1, kind: "escalated", task: "T1" }], state)).toEqual([]);
  });
});

describe("passItem", () => {
  it("is a toast only when the last pass failed, filed under that pass", () => {
    expect(passItem(snapshot([]))).toBeNull();
    const failed = snapshot([], { last_tick: { at_ms: 99, steps: 2, failed: [{ task: "T1", step: "spawn", error: "no slot" }, { task: "", step: "x", error: "y" }] } });
    expect(passItem(failed)).toMatchObject({ id: "tick:99", level: "error", body: "spawn on T1: no slot (+1)" });
    expect(fromSnapshot([], failed)).toHaveLength(1);
  });
});

describe("mergeNews", () => {
  const item = { id: "ev:1", level: "success" as const, title: "t", body: "b", task: "T1" };

  it("is one toast per event however often it is read", () => {
    const once = mergeNews([], [item], 1000);
    const twice = mergeNews(once, [item], 2000);
    expect(twice).toHaveLength(1);
    expect(twice[0]?.at).toBe(1000);
  });

  it("forgets what is stale, and keeps the newest few", () => {
    const old = mergeNews([], [item], 0);
    expect(mergeNews(old, [], KEEP_MS + 1)).toEqual([]);
    const many = mergeNews([], Array.from({ length: 9 }, (_, i) => ({ ...item, id: `ev:${i}` })), 1);
    expect(many).toHaveLength(5);
    expect(many[4]?.id).toBe("ev:8");
  });
});

describe("visible", () => {
  const mk = (id: string, level: "alert" | "success", at: number) => ({ id, level, title: id, body: "", task: "", at });

  it("shows at most three, drops the dismissed and the expired", () => {
    const list = ["a", "b", "c", "d"].map((id) => mk(id, "success", 0));
    expect(visible(list, 1000).map((t) => t.id)).toEqual(["b", "c", "d"]);
    expect(visible(list, 1000, new Set(["d"])).map((t) => t.id)).toEqual(["a", "b", "c"]);
    expect(SHOWN).toBe(3);
    expect(visible([mk("x", "success", 0)], 6_001)).toEqual([]);
    expect(visible([mk("x", "alert", 0)], 6_001)).toHaveLength(1);
    expect(timeoutOf({ level: "alert" })).toBe(20_000);
    expect(dismiss(list, "a")).toHaveLength(3);
  });

  it("cuts a body at 140 characters", () => {
    expect(clip("x".repeat(300)).length).toBe(140);
  });
});
