// The view arithmetic: a pinch, a wheel and a fit are the gestures a phone and
// a desktop use to get from a 3000 px drawing to the task they care about, and
// the only thing that can be wrong invisibly is where the anchor lands.

import { describe, expect, it } from "vitest";

import {
  IDENTITY,
  MAX_SCALE,
  MIN_SCALE,
  clampScale,
  fitTo,
  panBy,
  pinchView,
  toGraph,
  wheelFactor,
  zoomAt,
} from "./panzoom";

/** A graph point, drawn at `p * scale + (x, y)`. */
function screen(view: { x: number; y: number; scale: number }, x: number, y: number) {
  return { x: x * view.scale + view.x, y: y * view.scale + view.y };
}

describe("zoomAt", () => {
  it("keeps the point under the anchor where it was", () => {
    const start = { x: 40, y: -20, scale: 0.8 };
    const anchor = { x: 300, y: 180 };
    const graphPoint = toGraph(start, anchor);

    const zoomed = zoomAt(start, 1.5, anchor);
    const moved = screen(zoomed, graphPoint.x, graphPoint.y);

    expect(moved.x).toBeCloseTo(anchor.x);
    expect(moved.y).toBeCloseTo(anchor.y);
    expect(zoomed.scale).toBeCloseTo(start.scale * 1.5);
  });

  it("stops at the bounds instead of zooming for ever", () => {
    expect(zoomAt(IDENTITY, 100, { x: 0, y: 0 }).scale).toBe(MAX_SCALE);
    expect(zoomAt(IDENTITY, 0.0001, { x: 0, y: 0 }).scale).toBe(MIN_SCALE);
  });

  it("does nothing when the scale is already at its bound", () => {
    const at = zoomAt(IDENTITY, 100, { x: 10, y: 10 });
    expect(zoomAt(at, 100, { x: 10, y: 10 })).toBe(at);
  });
});

describe("panBy", () => {
  it("moves without scaling", () => {
    expect(panBy({ x: 5, y: 6, scale: 2 }, 10, -3)).toEqual({ x: 15, y: 3, scale: 2 });
  });
});

describe("pinchView", () => {
  it("scales by how far the fingers moved apart, around their midpoint", () => {
    const start = { x: 0, y: 0, scale: 1 };
    const from: [{ x: number; y: number }, { x: number; y: number }] = [
      { x: 100, y: 100 },
      { x: 200, y: 100 },
    ];
    const to: [{ x: number; y: number }, { x: number; y: number }] = [
      { x: 50, y: 100 },
      { x: 250, y: 100 },
    ];
    const view = pinchView(start, from, to);

    expect(view.scale).toBeCloseTo(2);
    // The graph point under the old midpoint (150, 100) is under the new one.
    const point = toGraph(start, { x: 150, y: 100 });
    const moved = screen(view, point.x, point.y);
    expect(moved.x).toBeCloseTo(150);
    expect(moved.y).toBeCloseTo(100);
  });

  it("ignores a pinch that starts where both fingers are", () => {
    const start = { x: 3, y: 4, scale: 1 };
    expect(
      pinchView(
        start,
        [
          { x: 10, y: 10 },
          { x: 10, y: 10 },
        ],
        [
          { x: 20, y: 20 },
          { x: 30, y: 30 },
        ],
      ),
    ).toBe(start);
  });
});

describe("fitTo", () => {
  it("centres a drawing that is bigger than the screen", () => {
    const view = fitTo({ width: 1200, height: 800 }, { width: 600, height: 400 }, 0);
    expect(view.scale).toBe(0.5);
    expect(view.x).toBe(0);
    expect(view.y).toBe(0);
  });

  it("never blows a small drawing up past its own size", () => {
    const view = fitTo({ width: 200, height: 100 }, { width: 1200, height: 800 }, 16);
    expect(view.scale).toBe(1);
    expect(view.x).toBe(500);
    expect(view.y).toBe(350);
  });

  it("keeps the padding on both sides", () => {
    const view = fitTo({ width: 800, height: 800 }, { width: 400, height: 400 }, 20);
    expect(view.scale).toBeCloseTo((400 - 40) / 800);
  });

  it("has an answer for a canvas that has not been measured yet", () => {
    expect(fitTo({ width: 100, height: 100 }, { width: 0, height: 0 })).toBe(IDENTITY);
    expect(fitTo({ width: 0, height: 0 }, { width: 600, height: 400 })).toBe(IDENTITY);
  });
});

describe("clampScale and wheelFactor", () => {
  it("keeps a scale usable", () => {
    expect(clampScale(0)).toBe(MIN_SCALE);
    expect(clampScale(99)).toBe(MAX_SCALE);
    expect(clampScale(Number.NaN)).toBe(1);
  });

  it("zooms in when the wheel goes up and out when it goes down", () => {
    expect(wheelFactor(-100)).toBeGreaterThan(1);
    expect(wheelFactor(100)).toBeLessThan(1);
    expect(wheelFactor(0)).toBe(1);
  });
});
