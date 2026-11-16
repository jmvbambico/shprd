// The drawing: the run's tasks and dependencies as an SVG, panned and zoomed.
//
// The layout is [`layoutGraph`]'s; this file only draws it and moves it. The
// screen it sits in decides what the data says (loading, no run, an error), and
// what a tap opens; the canvas hands back the task id that was tapped.
//
// Gestures are the ones a phone expects, as Shepherdr's window teaches them
// (MIT, `web/src/gestures.ts` at `a3f327a`): one finger drags the drawing, two
// pinch it, a wheel zooms a desktop, and a button puts it back on screen. A
// drag that ends over a node is a drag, never a tap, so panning does not open a
// task by accident.

import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";

import { evidenceMark, latestAttempt, taskTone, type RunTask } from "../api/rundoc";
import type { Layout } from "../layout/layout";
import { NODE, type GraphLayout, type GraphNode } from "./layout";
import {
  IDENTITY,
  fitTo,
  panBy,
  pinchView,
  wheelFactor,
  zoomAt,
  type Point,
  type Size,
  type View,
} from "./panzoom";

/** How far a finger may move and still be a tap, in canvas pixels. */
const TAP_SLOP = 6;

/** The width a desktop's detail card covers on the right of the canvas. */
const DETAIL_INSET = 364;

/** The drawing's padding when it is fitted to the screen. */
const FIT_PADDING = 20;

/** What the canvas needs from the screen. */
export interface GraphCanvasProps {
  /** The laid-out run. */
  layout: GraphLayout;
  /** The task whose detail is open, highlighted with its edges. */
  selected: string | null;
  /** Called with a task id when one is tapped, or `null` for the background. */
  onSelect: (id: string | null) => void;
  /** The layout class, so a phone can hide what does not fit. */
  device: Layout;
  /**
   * The viewport, when the caller can measure it and the environment cannot.
   *
   * A test has no layout engine: `getBoundingClientRect` is all zeroes there, so
   * the canvas would never fit the drawing. Passing a size is the same input the
   * browser gives it.
   */
  viewport?: Size;
}

/** The SVG of the run, with its gestures and the fit control. */
export function GraphCanvas({ layout, selected, onSelect, device, viewport }: GraphCanvasProps) {
  const container = useRef<HTMLDivElement | null>(null);
  const [measured, setMeasured] = useState<Size>({ width: 0, height: 0 });
  const [view, setView] = useState<View>(IDENTITY);
  // Whether the person has moved the drawing themselves. Until they do, the
  // view follows the window and the run; after that it is theirs.
  const touched = useRef(false);
  const size = viewport ?? measured;

  // The canvas is as big as its cell, which is not a size a person set: it is
  // measured, and measured again when the window changes. A browser without
  // `ResizeObserver` (a test) keeps the passed viewport or nothing.
  useEffect(() => {
    const node = container.current;
    if (node === null) {
      return;
    }
    const read = () => {
      const rect = node.getBoundingClientRect();
      setMeasured({ width: rect.width, height: rect.height });
    };
    read();
    if (typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(read);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  // The part of the canvas a drawing may use: on a desktop with a task open,
  // the right of the canvas is under the detail card, so fitting leaves that
  // strip alone rather than hiding half the run behind the card.
  const inset = device === "desktop" && selected !== null ? DETAIL_INSET : 0;

  const fit = useCallback(() => {
    touched.current = false;
    const room = { width: size.width - inset, height: size.height };
    if (room.width <= 0 || room.height <= 0) {
      return;
    }
    setView(fitTo({ width: layout.width, height: layout.height }, room, FIT_PADDING));
  }, [inset, layout.width, layout.height, size.width, size.height]);

  // A phone opens on the whole run rather than on its top-left corner, and a
  // window that grows, or a run that gains a task, is fitted again — but only
  // while the view is still the app's: once the person has panned or pinched, a
  // redraw must not move the drawing under their finger, and the fit button is
  // one tap away.
  useEffect(() => {
    if (touched.current || size.width <= 0 || layout.nodes.length === 0) {
      return;
    }
    fit();
  }, [fit, layout.nodes.length, size.width]);

  // A task opened from a node must be visible: on a desktop the detail covers
  // the right of the canvas, so the drawing moves just enough to bring the
  // selected node out from under it. It is a small pan, not a refit — every
  // other node stays where the person left it.
  useEffect(() => {
    if (selected === null || size.width <= 0) {
      return;
    }
    const node = layout.nodes.find((candidate) => candidate.task.id === selected);
    if (node === undefined) {
      return;
    }
    setView((current) => {
      const left = node.x * current.scale + current.x;
      const right = (node.x + NODE.width) * current.scale + current.x;
      const top = node.y * current.scale + current.y;
      const bottom = (node.y + NODE.height) * current.scale + current.y;
      const visible = size.width - inset;
      let dx = 0;
      let dy = 0;
      if (right > visible) {
        dx = visible - right - 12;
      }
      if (left + dx < 12) {
        dx = 12 - left;
      }
      if (bottom > size.height) {
        dy = size.height - bottom - 12;
      }
      if (top + dy < 12) {
        dy = 12 - top;
      }
      return dx === 0 && dy === 0 ? current : panBy(current, dx, dy);
    });
  }, [inset, selected, layout, size.width, size.height]);

  const pointers = useRef(new Map<number, Point>());
  const gesture = useRef<{ view: View; points: [Point, Point] | [Point] } | null>(null);
  // The pointers this gesture has captured, so a finger that leaves the canvas
  // keeps moving the drawing — and so a tap, which captures nothing, still
  // delivers its click to the node it started on.
  const captured = useRef(new Set<number>());
  // A drag that ended on a node leaves a click behind it: the flag outlives the
  // gesture (which `up` clears before the click arrives) so it can be ignored.
  const dragged = useRef(false);

  const local = (event: { clientX: number; clientY: number }): Point => {
    const node = container.current;
    if (node === null) {
      return { x: event.clientX, y: event.clientY };
    }
    const rect = node.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const down = (event: ReactPointerEvent<SVGSVGElement>) => {
    touched.current = true;
    const point = local(event);
    pointers.current.set(event.pointerId, point);
    const all = [...pointers.current.values()];
    if (all.length === 1) {
      dragged.current = false;
      gesture.current = { view, points: [point] };
    } else if (all.length === 2) {
      const [first, second] = all;
      if (first !== undefined && second !== undefined) {
        dragged.current = true;
        gesture.current = { view, points: [first, second] };
        // A pinch is the canvas's from the second finger on: capture both, so
        // either one leaving the canvas does not end it.
        for (const pointer of pointers.current.keys()) {
          capture(event.currentTarget, pointer, captured.current);
        }
      }
    }
  };

  const move = (event: ReactPointerEvent<SVGSVGElement>) => {
    if (!pointers.current.has(event.pointerId)) {
      return;
    }
    const point = local(event);
    pointers.current.set(event.pointerId, point);
    const current = gesture.current;
    if (current === null) {
      return;
    }
    const all = [...pointers.current.values()];
    if (all.length >= 2) {
      const [first, second] = all;
      const started = current.points;
      if (first !== undefined && second !== undefined && started.length === 2) {
        setView(pinchView(current.view, [started[0], started[1]], [first, second]));
      }
      return;
    }
    const from = current.points[0];
    if (all.length === 1 && from !== undefined) {
      const dx = point.x - from.x;
      const dy = point.y - from.y;
      if (Math.abs(dx) > TAP_SLOP || Math.abs(dy) > TAP_SLOP) {
        dragged.current = true;
        // Only once it is a drag: a tap that captured would have its click
        // delivered to the canvas rather than to the node it started on.
        capture(event.currentTarget, event.pointerId, captured.current);
      }
      setView(panBy(current.view, dx, dy));
    }
  };

  const up = (event: ReactPointerEvent<SVGSVGElement>) => {
    pointers.current.delete(event.pointerId);
    captured.current.delete(event.pointerId);
    const [remaining] = pointers.current.values();
    if (remaining === undefined) {
      gesture.current = null;
      return;
    }
    // A pinch that loses a finger becomes a drag from where the other one is,
    // not a jump back to where the pinch started.
    gesture.current = { view, points: [remaining] };
  };

  // A wheel is not a React event we can stop the page scrolling with: React
  // attaches it passively, so it is a native listener that may prevent default.
  useEffect(() => {
    const node = container.current;
    if (node === null) {
      return;
    }
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      touched.current = true;
      const rect = node.getBoundingClientRect();
      const anchor = { x: event.clientX - rect.left, y: event.clientY - rect.top };
      setView((current) => zoomAt(current, wheelFactor(event.deltaY), anchor));
    };
    node.addEventListener("wheel", onWheel, { passive: false });
    return () => node.removeEventListener("wheel", onWheel);
  }, []);

  const tap = (id: string | null) => {
    if (dragged.current) {
      return;
    }
    onSelect(id);
  };

  return (
    <div className="graph-canvas" ref={container} data-device={device}>
      <svg
        className="graph-svg"
        role="group"
        aria-label={`The run's graph: ${layout.nodes.length} tasks`}
        onPointerDown={down}
        onPointerMove={move}
        onPointerUp={up}
        onPointerCancel={up}
        onClick={(event) => {
          // Only the canvas itself clears the selection: a tap that bubbled up
          // from a node has already been handled by the node.
          if (event.target === event.currentTarget) {
            tap(null);
          }
        }}
      >
        <g transform={`translate(${view.x} ${view.y}) scale(${view.scale})`}>
          <g className="graph-edges" aria-hidden="true">
            {layout.edges.map((edge) => (
              <path
                key={`${edge.from}->${edge.to}`}
                className={
                  selected === null
                    ? "graph-edge"
                    : selected === edge.to
                      ? "graph-edge into-selected"
                      : selected === edge.from
                        ? "graph-edge from-selected"
                        : "graph-edge faded"
                }
                d={edge.path}
              />
            ))}
          </g>
          {layout.nodes.map((node) => (
            <NodeView
              key={node.task.id}
              node={node}
              selected={selected === node.task.id}
              dimmed={selected !== null && selected !== node.task.id}
              onTap={() => tap(node.task.id)}
            />
          ))}
        </g>
      </svg>
      <div className="graph-controls">
        <button type="button" className="graph-fit" onClick={fit}>
          Fit to screen
        </button>
      </div>
    </div>
  );
}

/** One task's box. */
function NodeView({
  node,
  selected,
  dimmed,
  onTap,
}: {
  node: GraphNode;
  selected: boolean;
  dimmed: boolean;
  onTap: () => void;
}) {
  const { task } = node;
  const state = taskTone(task.state);
  const attempt = latestAttempt(task);
  const mark = attempt?.outcome === undefined ? null : evidenceMark(attempt.outcome.evidence);
  const classes = [
    "graph-node",
    `tone-${state.tone}`,
    selected ? "selected" : "",
    dimmed ? "dimmed" : "",
  ]
    .filter((name) => name !== "")
    .join(" ");
  const label = `${task.id}: ${task.title}. ${state.word}.`;

  return (
    <g
      className={classes}
      transform={`translate(${node.x} ${node.y})`}
      role="button"
      tabIndex={0}
      aria-label={label}
      aria-pressed={selected}
      onClick={(event) => {
        // The canvas's own click clears the selection; a node's click must not
        // also reach it.
        event.stopPropagation();
        onTap();
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onTap();
        }
      }}
    >
      <rect className="graph-node-box" width={NODE.width} height={NODE.height} rx={10} />
      <text className="graph-node-id" x={14} y={21}>
        {task.id}
      </text>
      <text className="graph-node-title" x={14} y={42}>
        {short(task.title, 28)}
      </text>
      <text className="graph-node-state" x={14} y={64}>
        <tspan className="graph-dot" aria-hidden="true">
          ●
        </tspan>{" "}
        {state.word}
      </text>
      {task.attempts.length > 0 ? (
        <text className="graph-node-badge" x={NODE.width - 14} y={64} textAnchor="end">
          a{task.attempts.length}
          {mark === null ? "" : ` ${mark.glyph}`}
        </text>
      ) : null}
      <text className="graph-node-foot" x={14} y={82}>
        {foot(task)}
      </text>
      <title>{`${task.title} — ${state.word}${mark === null ? "" : `, ${mark.word}`}`}</title>
    </g>
  );
}

/** The node's last line: the owner, the note, or the first missing dependency. */
function foot(task: RunTask): string {
  if (task.owner !== undefined && task.owner !== "") {
    return task.owner;
  }
  if (task.note !== undefined && task.note !== "") {
    return short(task.note, 32);
  }
  if (task.deps.length > 0) {
    return `waits ${task.deps.join(", ")}`;
  }
  return task.kind;
}

/** Captures one pointer, once; a browser without capture still gets the moves. */
function capture(target: Element, pointerId: number, done: Set<number>): void {
  if (done.has(pointerId)) {
    return;
  }
  done.add(pointerId);
  try {
    (target as Element & { setPointerCapture?: (id: number) => void }).setPointerCapture?.(pointerId);
  } catch {
    // A synthetic pointer cannot be captured; the gesture works without it.
  }
}

/** A line cut to `max` characters, with an ellipsis when it was. */
function short(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}
