// The breakpoints are numbers a phone, a tablet and a laptop are placed by, so
// they are pinned here as well as in `styles.css`.

import { describe, expect, it } from "vitest";

import {
  DESKTOP_MIN,
  PHONE_MAX,
  SCREENS,
  SCREEN_LABEL,
  columnsFor,
  layoutFor,
  type Layout,
} from "./layout";

describe("layoutFor", () => {
  const cases: Array<[number, Layout]> = [
    [320, "phone"],
    [390, "phone"],
    [799, "phone"],
    [800, "tablet"],
    [900, "tablet"],
    [1023, "tablet"],
    [1024, "desktop"],
    [1440, "desktop"],
    [2560, "desktop"],
  ];

  for (const [width, layout] of cases) {
    it(`${width} px is ${layout}`, () => {
      expect(layoutFor(width)).toBe(layout);
    });
  }

  it("a phone and a desktop are the widths the task names", () => {
    expect(PHONE_MAX).toBe(800);
    expect(DESKTOP_MIN).toBe(1024);
    expect(layoutFor(PHONE_MAX - 1)).toBe("phone");
    expect(layoutFor(DESKTOP_MIN - 1)).toBe("tablet");
    expect(layoutFor(DESKTOP_MIN)).toBe("desktop");
  });
});

describe("columnsFor", () => {
  it("draws one, two and three columns", () => {
    expect(columnsFor("phone")).toBe(1);
    expect(columnsFor("tablet")).toBe(2);
    expect(columnsFor("desktop")).toBe(3);
  });

  it("the columns match the widths the task names", () => {
    expect(columnsFor(layoutFor(390))).toBe(1);
    expect(columnsFor(layoutFor(900))).toBe(2);
    expect(columnsFor(layoutFor(1440))).toBe(3);
  });
});

describe("screens", () => {
  it("are the five this task stubs", () => {
    expect(SCREENS).toEqual(["tasks", "graph", "terminal", "chat", "settings"]);
  });

  it("each has a word for its tab", () => {
    for (const screen of SCREENS) {
      expect(SCREEN_LABEL[screen]).not.toBe("");
    }
  });
});
