// The graph's geometry: a layered layout, left to right by dependency depth.
//
// dagr draws a run as a tree; a browser draws it as a directed graph, so this
// fits the document's `deps` into layers: every task sits one column to the
// right of its deepest dependency, tasks with no dependencies sit in the first
// column, and the edges run left to right. It is the smallest layout that keeps
// the one thing a person reads off a graph — what waits on what — true on
// screen.
//
// Everything here is a pure function of the document, because the drawing is
// where a mistake would be invisible: a cycle, a dependency naming a task the
// run does not have, a column nobody ordered. Unknown dependencies are kept on
// the node (the detail says so) and never invented as edges; a cycle is drawn
// flat rather than recursing for ever.
//
// The drawing's own sizes are here too, so the canvas, the stylesheet and the
// tests cannot drift apart.

import type { RunTask } from "../api/rundoc";

/** One node's box, and where it sits in the grid. */
export interface GraphNode {
  /** The task it draws. */
  task: RunTask;
  /** Its dependency depth: 0 for a task nothing blocks. */
  column: number;
  /** Its place in that column. */
  row: number;
  /** The box's left edge, in graph coordinates. */
  x: number;
  /** The box's top edge, in graph coordinates. */
  y: number;
  /** Its dependencies that are not in this run, so the detail can name them. */
  missingDeps: string[];
}

/** One dependency edge between two tasks, with the path to draw it. */
export interface GraphEdge {
  /** The dependency's task id. */
  from: string;
  /** The dependent task's id. */
  to: string;
  /** The SVG path, in graph coordinates. */
  path: string;
}

/** Everything the canvas draws, in graph coordinates. */
export interface GraphLayout {
  /** Every task, in column then row order. */
  nodes: GraphNode[];
  /** Every dependency between two tasks of this run. */
  edges: GraphEdge[];
  /** The drawing's width, margins included. */
  width: number;
  /** The drawing's height, margins included. */
  height: number;
  /** How many columns the drawing has (0 for an empty run). */
  columns: number;
}

/** One node's size. Big enough for an id, a title and two badges. */
export const NODE = { width: 216, height: 92 } as const;

/** The space between columns, and between rows. */
export const GAP = { x: 76, y: 24 } as const;

/** The space around the drawing, so a node is never on the canvas edge. */
export const MARGIN = 24;

/** Lays a run's tasks out, left to right by dependency depth. */
export function layoutGraph(tasks: readonly RunTask[]): GraphLayout {
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const depths = depthsOf(tasks, byId);

  // Group by column, then order each column the way a person reads it: a node
  // sits near the dependencies it waits on (the mean row of those), and a task
  // nothing blocks keeps its id order. One left-to-right pass is enough because
  // a dependency is always in a column already placed.
  const columns: RunTask[][] = [];
  for (const task of tasks) {
    const depth = depths.get(task.id) ?? 0;
    (columns[depth] ??= []).push(task);
  }
  const rows = new Map<string, number>();
  for (const [column, members] of columns.entries()) {
    const keyed = members.map((task) => ({
      task,
      key: barycentre(task, byId, rows),
    }));
    keyed.sort((left, right) => left.key - right.key || left.task.id.localeCompare(right.task.id));
    for (const [row, entry] of keyed.entries()) {
      rows.set(entry.task.id, row);
    }
    // Write the sorted order back, so the node list is in drawing order.
    columns[column] = keyed.map((entry) => entry.task);
  }

  const nodes: GraphNode[] = [];
  for (const [column, members] of columns.entries()) {
    for (const [row, task] of members.entries()) {
      nodes.push({
        task,
        column,
        row,
        x: MARGIN + column * (NODE.width + GAP.x),
        y: MARGIN + row * (NODE.height + GAP.y),
        missingDeps: task.deps.filter((dep) => !byId.has(dep)),
      });
    }
  }

  const placed = new Map(nodes.map((node) => [node.task.id, node]));
  const edges: GraphEdge[] = [];
  for (const node of nodes) {
    for (const dep of node.task.deps) {
      const source = placed.get(dep);
      if (source === undefined || source.task.id === node.task.id) {
        continue;
      }
      edges.push({ from: dep, to: node.task.id, path: edgePath(source, node) });
    }
  }

  return {
    nodes,
    edges,
    columns: columns.length,
    width: 2 * MARGIN + columns.length * NODE.width + Math.max(columns.length - 1, 0) * GAP.x,
    height: 2 * MARGIN + rowsIn(columns) * NODE.height + (rowsIn(columns) - 1) * GAP.y,
  };
}

/** The number of rows the tallest column draws: at least one, even when empty. */
function rowsIn(columns: readonly RunTask[][]): number {
  let rows = 0;
  for (const members of columns) {
    rows = Math.max(rows, members.length);
  }
  return Math.max(rows, 1);
}

/** Every task a node depends on, as the detail lists them. */
export function waitsOn(task: RunTask, tasks: readonly RunTask[]): RunTask[] {
  const byId = new Map(tasks.map((other) => [other.id, other]));
  return task.deps.flatMap((dep) => {
    const found = byId.get(dep);
    return found === undefined ? [] : [found];
  });
}

/**
 * Every task's dependency depth: how many edges the longest chain back to a task
 * with no dependencies has.
 *
 * A cycle cannot be laid out, and the projection never writes one (`dagr check`
 * refuses them), but a hand-edited document can: a task already on the stack is
 * treated as depth 0 instead of recursing, so the drawing is flat rather than
 * infinite.
 */
function depthsOf(
  tasks: readonly RunTask[],
  byId: ReadonlyMap<string, RunTask>,
): Map<string, number> {
  const depths = new Map<string, number>();
  const onStack = new Set<string>();
  const depthOf = (id: string): number => {
    const known = depths.get(id);
    if (known !== undefined) {
      return known;
    }
    const task = byId.get(id);
    if (task === undefined || onStack.has(id)) {
      return 0;
    }
    onStack.add(id);
    let depth = 0;
    for (const dep of task.deps) {
      if (dep === id || !byId.has(dep) || onStack.has(dep)) {
        continue;
      }
      depth = Math.max(depth, depthOf(dep) + 1);
    }
    onStack.delete(id);
    depths.set(id, depth);
    return depth;
  };
  for (const task of tasks) {
    depthOf(task.id);
  }
  return depths;
}

/** The mean row of a task's placed dependencies, or `Infinity` for none. */
function barycentre(
  task: RunTask,
  byId: ReadonlyMap<string, RunTask>,
  rows: ReadonlyMap<string, number>,
): number {
  let sum = 0;
  let count = 0;
  for (const dep of task.deps) {
    if (!byId.has(dep)) {
      continue;
    }
    const row = rows.get(dep);
    if (row !== undefined) {
      sum += row;
      count += 1;
    }
  }
  return count === 0 ? Number.POSITIVE_INFINITY : sum / count;
}

/** The path from a dependency's right edge to its dependent's left edge. */
function edgePath(from: GraphNode, to: GraphNode): string {
  const x1 = from.x + NODE.width;
  const y1 = from.y + NODE.height / 2;
  const x2 = to.x;
  const y2 = to.y + NODE.height / 2;
  // A dependency in the same or a later column (only a cycle or a duplicate can
  // do that) is routed out to the right first, so the edge is visible rather
  // than drawn backwards through both boxes.
  if (x2 <= x1) {
    const out = x1 + GAP.x / 2;
    return `M ${round(x1)} ${round(y1)} C ${round(out)} ${round(y1)}, ${round(out)} ${round(y2)}, ${round(x2)} ${round(y2)}`;
  }
  const bend = Math.max((x2 - x1) / 2, 18);
  return `M ${round(x1)} ${round(y1)} C ${round(x1 + bend)} ${round(y1)}, ${round(x2 - bend)} ${round(y2)}, ${round(x2)} ${round(y2)}`;
}

/** A coordinate, to a tenth of a pixel: enough to be smooth, short on the wire. */
function round(value: number): number {
  return Math.round(value * 10) / 10;
}

