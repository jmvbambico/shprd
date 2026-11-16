// The drawing's own behaviour: what a tap means, and what a drag means. The
// arithmetic is in `panzoom.test.ts`; this is the wiring a finger touches.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { RunTask } from "../api/rundoc";
import { GraphCanvas } from "./GraphCanvas";
import { layoutGraph } from "./layout";

/** A task with the fields the canvas reads. */
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

/** The canvas, fitted to a viewport a test can state. */
function draw(tasks: RunTask[]) {
  const layout = layoutGraph(tasks);
  const selected: (string | null)[] = [];
  const view = render(
    <GraphCanvas
      layout={layout}
      selected={null}
      onSelect={(id) => selected.push(id)}
      device="phone"
      viewport={{ width: 800, height: 600 }}
    />,
  );
  return { ...view, layout, selected };
}

/** The drawing's transform, as the SVG carries it. */
function transform(container: HTMLElement): string {
  const group = container.querySelector("svg > g");
  return group?.getAttribute("transform") ?? "";
}

/** The SVG element. */
function svg(container: HTMLElement): SVGSVGElement {
  const element = container.querySelector("svg");
  if (element === null) {
    throw new Error("the canvas drew no SVG");
  }
  return element;
}

afterEach(cleanup);

describe("GraphCanvas", () => {
  it("draws a node per task and an edge per dependency", () => {
    const { container } = draw([task("A"), task("B", ["A"]), task("C", ["B"])]);
    expect(screen.getByRole("button", { name: "A: task A. Queued." })).toBeDefined();
    expect(screen.getByRole("button", { name: "B: task B. Queued." })).toBeDefined();
    expect(container.querySelectorAll("path.graph-edge")).toHaveLength(2);
  });

  it("badges a node with its attempts and their evidence", () => {
    draw([
      task("A", [], {
        state: "working",
        attempts: [
          {
            id: "A·a1",
            n: 1,
            cause: { type: "initial" },
            actor: "pi",
            state: "done",
            started_at: "2026-11-16T10:00:00Z",
            ended_at: "2026-11-16T10:01:00Z",
            outcome: { result: "done", evidence: "verified" },
          },
        ],
      }),
    ]);
    expect(screen.getByText("a1 ◆")).toBeDefined();
  });

  it("opens the task a node is tapped", () => {
    const { selected } = draw([task("A"), task("B", ["A"])]);
    fireEvent.click(screen.getByRole("button", { name: "B: task B. Queued." }));
    expect(selected).toEqual(["B"]);
  });

  it("clears the selection when the background is tapped", () => {
    const { selected, container } = draw([task("A")]);
    fireEvent.click(svg(container));
    expect(selected).toEqual([null]);
  });

  it("pans with one finger", () => {
    const { container } = draw([task("A")]);
    const before = transform(container);
    const canvas = svg(container);

    fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 160, clientY: 140 });
    fireEvent.pointerUp(canvas, { pointerId: 1, clientX: 160, clientY: 140 });

    expect(transform(container)).not.toBe(before);
  });

  it("zooms with two fingers", () => {
    const { container } = draw([task("A")]);
    const canvas = svg(container);
    const scale = () => Number(/scale\(([\d.]+)\)/.exec(transform(container))?.[1]);

    fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 200, clientY: 200 });
    fireEvent.pointerDown(canvas, { pointerId: 2, clientX: 300, clientY: 200 });
    const before = scale();
    fireEvent.pointerMove(canvas, { pointerId: 2, clientX: 400, clientY: 200 });

    expect(scale()).toBeGreaterThan(before);
  });

  it("does not open the task a drag ended over", () => {
    const { selected, container } = draw([task("A")]);
    const canvas = svg(container);
    const node = screen.getByRole("button", { name: "A: task A. Queued." });

    // A finger goes down on the node, moves the drawing, and lifts: the click
    // the browser adds afterwards is not a tap.
    fireEvent.pointerDown(node, { pointerId: 1, clientX: 20, clientY: 20 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 120, clientY: 90 });
    fireEvent.pointerUp(canvas, { pointerId: 1, clientX: 120, clientY: 90 });
    fireEvent.click(node);

    expect(selected).toEqual([]);
  });

  it("brings a selected node out from under the desktop's detail card", () => {
    const layout = layoutGraph([task("A"), task("B", ["A"]), task("C", ["B"]), task("D", ["C"])]);
    const { container, rerender } = render(
      <GraphCanvas
        layout={layout}
        selected={null}
        onSelect={() => undefined}
        device="desktop"
        viewport={{ width: 1440, height: 700 }}
      />,
    );
    const before = transform(container);
    rerender(
      <GraphCanvas
        layout={layout}
        selected="D"
        onSelect={() => undefined}
        device="desktop"
        viewport={{ width: 1440, height: 700 }}
      />,
    );
    const after = transform(container);
    expect(after).not.toBe(before);

    // The node's right edge is now left of the card.
    const view = /translate\(([-\d.]+) ([-\d.]+)\) scale\(([\d.]+)\)/.exec(after);
    expect(view).not.toBeNull();
    if (view !== null) {
      const [, x, , scale] = view;
      const node = layout.nodes.find((candidate) => candidate.task.id === "D");
      expect(node).toBeDefined();
      if (node !== undefined) {
        const right = (node.x + 216) * Number(scale) + Number(x);
        expect(right).toBeLessThanOrEqual(1440 - 364);
      }
    }

    // Fit leaves the strip under the card alone too.
    fireEvent.click(screen.getByRole("button", { name: "Fit to screen" }));
    const fitted = /translate\(([-\d.]+) ([-\d.]+)\) scale\(([\d.]+)\)/.exec(transform(container));
    expect(fitted).not.toBeNull();
    if (fitted !== null) {
      const [, x, , scale] = fitted;
      const node = layout.nodes.find((candidate) => candidate.task.id === "D");
      if (node !== undefined) {
        const right = (node.x + 216) * Number(scale) + Number(x);
        expect(right).toBeLessThanOrEqual(1440 - 364);
      }
    }
  });

  it("puts the drawing back on screen when Fit is pressed", () => {
    const { container } = draw([task("A"), task("B", ["A"]), task("C", ["B"])]);
    const fitted = transform(container);
    const canvas = svg(container);

    fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 400, clientY: 300 });
    fireEvent.pointerUp(canvas, { pointerId: 1, clientX: 400, clientY: 300 });
    expect(transform(container)).not.toBe(fitted);

    fireEvent.click(screen.getByRole("button", { name: "Fit to screen" }));
    expect(transform(container)).toBe(fitted);
  });
});
