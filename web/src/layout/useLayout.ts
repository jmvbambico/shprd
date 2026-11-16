// React's view of the layout breakpoints in `layout.ts`.

import { useEffect, useState } from "react";

import { layoutFor, type Layout } from "./layout";

/** The viewport width, re-read when the window resizes. */
function currentWidth(): number {
  return typeof globalThis.innerWidth === "number" ? globalThis.innerWidth : 1024;
}

/**
 * The layout this window gets, kept in step with `resize`.
 *
 * A phone that turns sideways, or a desktop window that is dragged narrow,
 * changes class on the next frame; the value is read once at mount so the first
 * paint is already right.
 */
export function useLayout(): Layout {
  const [width, setWidth] = useState(currentWidth);

  useEffect(() => {
    const onResize = () => setWidth(currentWidth());
    globalThis.addEventListener("resize", onResize);
    return () => globalThis.removeEventListener("resize", onResize);
  }, []);

  return layoutFor(width);
}
