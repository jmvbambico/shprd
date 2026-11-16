// The layered layout: what waits on what has to read off the screen, so these
// pin the columns, the edges, and the two documents a hand edit can produce (a
// cycle and a dependency naming a task the run does not have).

import { describe, expect, it } from "vitest";

import type { RunTask } from "../api/rundoc";

import { GAP, MARGIN, NODE, layoutGraph, waitsOn } from "./layout";

/** A task with the fields the layout reads. */
function task(id: string, deps: string[] = [], extra: Partial<RunTask> = {}): RunTask {
  return {
    id,
    title: `task ${id}`,
    kind: "impl",
    state: "queued",
    deps,
    criteria: "",
    attempts: [],
    ...extra,
  };
}

describe("layoutGraph", () => {
  it("puts a task one column right of its deepest dependency", () => {
    const layout = layoutGraph([task("A"), task("B", ["A"]), task("C", ["B"]), task("D", ["A", "C"])]);
    const column = (id: string) => layout.nodes.find((node) => node.task.id === id)?.column;
    expect(column("A")).toBe(0);
    expect(column("B")).toBe(1);
    expect(column("C")).toBe(2);
    expect(column("D")).toBe(3);
    expect(layout.columns).toBe(4);
  });

  it("draws one edge per dependency, from the dependency to the task", () => {
    const layout = layoutGraph([task("A"), task("B", ["A"])]);
    expect(layout.edges).toHaveLength(1);
    expect(layout.edges[0]?.from).toBe("A");
    expect(layout.edges[0]?.to).toBe("B");
    expect(layout.edges[0]?.path.startsWith("M ")).toBe(true);
  });

  it("sizes the drawing to its columns and rows, with a margin", () => {
    const one = layoutGraph([task("A")]);
    expect(one.width).toBe(2 * MARGIN + NODE.width);
    expect(one.height).toBe(2 * MARGIN + NODE.height);

    const column = layoutGraph([task("A"), task("B", ["A"])]);
    expect(column.width).toBe(2 * MARGIN + 2 * NODE.width + GAP.x);
    expect(column.height).toBe(2 * MARGIN + NODE.height);
  });

  it("keeps a dependency on a task this run does not have, without inventing it", () => {
    const layout = layoutGraph([task("A", ["GHOST"])]);
    expect(layout.edges).toEqual([]);
    expect(layout.nodes[0]?.missingDeps).toEqual(["GHOST"]);
    expect(layout.nodes[0]?.column).toBe(0);
  });

  it("draws a cycle flat instead of recursing for ever", () => {
    // `dagr check` refuses a cycle, but a hand-edited file can hold one, and a
    // page that never returns is worse than a page that draws it oddly.
    const layout = layoutGraph([task("A", ["B"]), task("B", ["A"])]);
    expect(layout.nodes).toHaveLength(2);
    expect(layout.edges).toHaveLength(2);
    for (const node of layout.nodes) {
      expect(Number.isFinite(node.column)).toBe(true);
      expect(Number.isFinite(node.row)).toBe(true);
    }
  });

  it("puts independent tasks in the first column, in id order", () => {
    const layout = layoutGraph([task("C"), task("A"), task("B")]);
    expect(layout.nodes.map((node) => node.task.id)).toEqual(["A", "B", "C"]);
  });

  it("orders a column the way its dependencies are drawn", () => {
    // In the first column A is above B, so the task waiting on A is drawn above
    // the one waiting on B, and the two edges do not cross.
    const layout = layoutGraph([task("A"), task("B"), task("C", ["A"]), task("D", ["B"])]);
    const row = (id: string) => layout.nodes.find((node) => node.task.id === id)?.row;
    expect(row("C")).toBe(0);
    expect(row("D")).toBe(1);
  });

  it("lays an empty run out as an empty drawing", () => {
    const layout = layoutGraph([]);
    expect(layout.nodes).toEqual([]);
    expect(layout.edges).toEqual([]);
    expect(layout.columns).toBe(0);
    expect(layout.width).toBe(2 * MARGIN);
    expect(layout.height).toBe(2 * MARGIN + NODE.height);
  });
});

describe("waitsOn", () => {
  it("finds the tasks a dependency list names, in order", () => {
    const tasks = [task("A"), task("B"), task("C", ["B", "A", "GHOST"])];
    const found = waitsOn(tasks[2]!, tasks).map((dep) => dep.id);
    expect(found).toEqual(["B", "A"]);
  });
});
