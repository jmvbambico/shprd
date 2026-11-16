// The canvas's view: how the drawing is moved and scaled under the person's
// fingers.
//
// A graph is bigger than a phone. Shepherdr's window teaches the gesture set
// this follows (MIT, `web/src/gestures.ts` and `web/src/viewport.ts` at
// `a3f327a`): one finger drags, two fingers pinch, a mouse wheel zooms, and a
// control puts the whole thing back on screen. Its swipes are for moving between
// screens and are not this: nothing here navigates.
//
// All of it is arithmetic on one [`View`], so a pinch or a wheel zoom can be
// tested without a browser. The transform is the SVG one the canvas applies:
// a graph point `p` is drawn at `p * scale + (x, y)`.

/** A point, in the canvas's own pixels. */
export interface Point {
  /** Across. */
  x: number;
  /** Down. */
  y: number;
}

/** Where the drawing sits and how big it is. */
export interface View {
  /** How far the drawing is moved right, in canvas pixels. */
  x: number;
  /** How far it is moved down. */
  y: number;
  /** How much it is scaled: 1 draws graph coordinates as pixels. */
  scale: number;
}

/** The smallest a drawing may be zoomed out to. Below this, words stop reading. */
export const MIN_SCALE = 0.25;

/** The largest a drawing may be zoomed in to. */
export const MAX_SCALE = 2.5;

/** The drawing at its own size, at the top left. */
export const IDENTITY: View = { x: 0, y: 0, scale: 1 };

/** A size, in pixels. */
export interface Size {
  /** Across. */
  width: number;
  /** Down. */
  height: number;
}

/** Moves the drawing by a drag, in canvas pixels. */
export function panBy(view: View, dx: number, dy: number): View {
  return { x: view.x + dx, y: view.y + dy, scale: view.scale };
}

/**
 * Zooms by `factor` around a point on screen, so whatever is under the finger or
 * the cursor stays under it.
 *
 * That is the whole trick of a pinch: the graph point under the anchor is found
 * from the old view, and the new translation puts it back under the same anchor.
 */
export function zoomAt(view: View, factor: number, anchor: Point): View {
  const scale = clampScale(view.scale * factor);
  if (scale === view.scale) {
    return view;
  }
  const point = toGraph(view, anchor);
  return { x: anchor.x - point.x * scale, y: anchor.y - point.y * scale, scale };
}

/**
 * The view two fingers describe: the drawing under their midpoint at the start
 * is under their midpoint now, scaled by how much further apart they went.
 */
export function pinchView(
  start: View,
  from: readonly [Point, Point],
  to: readonly [Point, Point],
): View {
  const fromSpan = span(from);
  const toSpan = span(to);
  if (fromSpan === 0) {
    return start;
  }
  const scale = clampScale(start.scale * (toSpan / fromSpan));
  const middle = midpoint(to);
  const point = toGraph(start, midpoint(from));
  return { x: middle.x - point.x * scale, y: middle.y - point.y * scale, scale };
}

/**
 * The view that puts `content` in the middle of `viewport`, as large as it fits.
 *
 * A drawing narrower than the viewport is not blown up past its own size: a
 * graph of three tasks at phone width should read, not fill the screen with two
 * words.
 */
export function fitTo(content: Size, viewport: Size, padding = 16): View {
  if (content.width <= 0 || content.height <= 0 || viewport.width <= 0 || viewport.height <= 0) {
    return IDENTITY;
  }
  const scale = clampScale(
    Math.min(
      1,
      (viewport.width - 2 * padding) / content.width,
      (viewport.height - 2 * padding) / content.height,
    ),
  );
  return {
    x: (viewport.width - content.width * scale) / 2,
    y: (viewport.height - content.height * scale) / 2,
    scale,
  };
}

/** The drawing's own coordinates for a point on screen. */
export function toGraph(view: View, point: Point): Point {
  return { x: (point.x - view.x) / view.scale, y: (point.y - view.y) / view.scale };
}

/** A scale kept between [`MIN_SCALE`] and [`MAX_SCALE`]. */
export function clampScale(scale: number): number {
  if (!Number.isFinite(scale)) {
    return 1;
  }
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

/** The zoom a wheel delta asks for: a notch in, a notch out, never a jump. */
export function wheelFactor(deltaY: number): number {
  return Math.exp(-deltaY * 0.0015);
}

/** The distance between two points. */
function span(points: readonly [Point, Point]): number {
  const dx = points[1].x - points[0].x;
  const dy = points[1].y - points[0].y;
  return Math.hypot(dx, dy);
}

/** The point halfway between two. */
function midpoint(points: readonly [Point, Point]): Point {
  return { x: (points[0].x + points[1].x) / 2, y: (points[0].y + points[1].y) / 2 };
}
