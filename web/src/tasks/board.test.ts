// The board's rules: grouping, the filter row, the search, a row's one act and
// the add-task form's reading — the same rules the Herdr panel follows.

import { describe, expect, it } from "vitest";

import { counts, filterChips, found, listView, newTask, rowActions, secondLine, taskById } from "./board";
import { snapshot, task } from "./fixtures";

const mixed = snapshot([
  task("T1", "needs_you", { derived: { status: "needs_you", need: "asks" } }),
  task("T2", "ready"),
  task("T3", "working", { last_status: { message: "wiring the route", progress: 40, seq: 2 } }),
  task("T4", "queued", { last_status: undefined }),
  task("T5", "done"),
  task("T6", "done"),
  task("T7", "to_do"),
]);

describe("secondLine", () => {
  it("says why it needs you, then the agent's progress, then the title", () => {
    expect(secondLine(mixed.tasks[0])).toBe("Asks you something");
    expect(secondLine(mixed.tasks[2])).toBe("wiring the route");
    expect(secondLine(mixed.tasks[3])).toBe("Title of T4");
  });

  it("keeps the engine's whole Waiting line, with the path and the task in it", () => {
    const line = "Waiting: crates/shprd/tests is in use by install-layout-source";
    expect(secondLine(task("W", "to_do", { last_status: { message: line, seq: 1 } }))).toBe(line);
  });
});

describe("listView", () => {
  it("puts needs-you then ready under For you, and folds Done to one line", () => {
    const view = listView(mixed);
    expect(view.groups.map((g) => g.key)).toEqual(["you", "working", "queued", "to_do"]);
    expect(view.groups[0]?.tasks.map((t) => t.id)).toEqual(["T1", "T2"]);
    expect(view.doneLine).toEqual({ count: 2, label: "2 done" });
  });

  it("opens Done when asked, and shows only the newest few until Show more", () => {
    const many = snapshot(Array.from({ length: 8 }, (_, i) => task(`D${i}`, "done")));
    const open = listView(many, { doneOpen: true });
    const done = open.groups.find((g) => g.key === "done");
    expect(done?.tasks).toHaveLength(5);
    expect(done?.cut).toBe(3);
    expect(listView(many, { doneOpen: true, allDone: true }).groups[0]?.tasks).toHaveLength(8);
  });

  it("a status word shows that word's tasks flat", () => {
    const view = listView(mixed, { filter: "working" });
    expect(view.groups).toHaveLength(1);
    expect(view.groups[0]).toMatchObject({ flat: true, label: "Working" });
    expect(view.doneLine).toBeNull();
  });

  it("a query finds across Done and never folds a heading", () => {
    const view = listView(mixed, { query: "t5", shut: new Set(["done"]) });
    expect(view.groups[0]?.tasks.map((t) => t.id)).toEqual(["T5"]);
    expect(view.groups[0]?.shut).toBe(false);
    expect(view.doneLine).toBeNull();
  });

  it("leaves a dropped task out: it is history", () => {
    const view = listView(snapshot([task("X", "queued", { status: "dropped" })]));
    expect(view.groups).toEqual([]);
  });

  it("folds a heading the person shut", () => {
    expect(listView(mixed, { shut: new Set(["working"]) }).groups.find((g) => g.key === "working")?.shut).toBe(true);
  });
});

describe("search", () => {
  it("finds any words in any order, by id, title, second line, status word or PR", () => {
    expect(found(mixed.tasks[2]!, "wiring route")).toBe(true);
    expect(found(mixed.tasks[2]!, "working")).toBe(true);
    expect(found(mixed.tasks[2]!, "nothing like it")).toBe(false);
    const withPr = task("P", "ready", {
      pr: { number: 42, url: "https://x/42", head: "feature/p", state: "open", approved: false },
    });
    expect(found(withPr, "#42")).toBe(true);
  });
});

describe("filter chips", () => {
  it("draws needs you, working and ready when they have rows, the rest under More", () => {
    const chips = filterChips(mixed);
    expect(chips.primary.map((c) => c.key)).toEqual(["needs_you", "working", "ready"]);
    expect(chips.more.map((c) => c.key)).toEqual(["queued", "to_do", "done"]);
    expect(counts(mixed)).toEqual({ needs: 1, ready: 1, total: 7 });
  });
});

describe("rowActions", () => {
  it("offers the one act the engine would accept", () => {
    expect(rowActions(task("a", "ready"))[0]?.id).toBe("review");
    expect(rowActions(task("a", "to_do"))[0]).toMatchObject({ id: "start", action: "task.start" });
    expect(rowActions(task("a", "working"))[0]).toMatchObject({ id: "stop", action: "task.stop" });
    expect(rowActions(task("a", "queued", { status: "failed" }))[0]).toMatchObject({ id: "retry" });
    expect(rowActions(task("a", "done"))).toEqual([]);
    expect(rowActions(task("a", "queued", { status: "dropped" }))).toEqual([]);
  });
});

describe("newTask", () => {
  it("says what is missing, in order, then is ready", () => {
    expect(newTask({}).why).toBe("A task needs a title");
    expect(newTask({ title: "t" }).why).toBe("Name the folder or path it may change");
    expect(newTask({ title: "t", scope: "a" }).why).toBe("Say what must be true when it is done");
    const ok = newTask({ title: " t ", scope: "a, b", acceptance: "x; y" });
    expect(ok).toMatchObject({ ready: true, title: "t", scope: ["a", "b"], acceptance: ["x", "y"] });
  });
});

describe("taskById", () => {
  it("answers null for a task that left the board", () => {
    expect(taskById(mixed, "T1")?.id).toBe("T1");
    expect(taskById(mixed, "gone")).toBeNull();
    expect(taskById(mixed, null)).toBeNull();
  });
});
