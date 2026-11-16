// The graph screen: the run as the Herdr window's graph block draws it.
//
// The desktop's block is a terminal running `dagr view <repo>/.dagr/run.json`,
// and the service keeps that document fresh after every ledger write. The web
// cannot run dagr, so this screen draws the same document itself: tasks as nodes
// with their dagr state, dependencies as edges, the attempt count and the
// evidence that settled the latest one as badges. It redraws on every change
// notice, so a run that moves on the desktop moves here too.
//
// The words for a state this run is not in are the ones the graph block uses
// (`shprd-graph`'s `view.js`): *No run yet* names `shprd run start`, and the
// missing-document message names the file. Nothing here invents a fact the
// document does not carry.

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiClient } from "../api/client";
import { parseRunDocument, type RunDocument } from "../api/rundoc";
import { useLiveRefresh } from "../api/useLive";
import { useLayout } from "../layout/useLayout";
import { GraphCanvas } from "./GraphCanvas";
import { count, when } from "./format";
import { layoutGraph } from "./layout";
import { TaskDetail } from "./TaskDetail";

/**
 * How long after a change notice the document is read, ms.
 *
 * The service publishes the notice before the binary's post-write step rewrites
 * `<repo>/.dagr/run.json`, so a read on the instant can catch the previous
 * document. A short wait also coalesces a burst of writes into one read.
 */
const SETTLE_MS = 200;

/** What the screen is showing. */
type GraphState =
  | { phase: "loading" }
  | { phase: "empty"; lead: string; message: string }
  | { phase: "error"; message: string }
  | { phase: "ready"; document: RunDocument; problem?: string };

/** The graph screen. */
export function GraphScreen({ client }: { client: ApiClient }) {
  const device = useLayout();
  const [state, setState] = useState<GraphState>({ phase: "loading" });
  const [selected, setSelected] = useState<string | null>(null);
  // The latest state, for the loader: a refresh that fails keeps what is drawn,
  // and a loader rebuilt on every state change would restart the feed.
  const latest = useRef<GraphState>(state);
  const loadId = useRef(0);
  const settle = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(async () => {
    const id = ++loadId.current;
    const next = await read(client, latest.current);
    if (id !== loadId.current) {
      return;
    }
    latest.current = next;
    setState(next);
    // A task that left the run closes its detail rather than showing a node the
    // graph no longer has.
    setSelected((current) =>
      current === null || next.phase !== "ready"
        ? null
        : next.document.tasks.some((task) => task.id === current)
          ? current
          : null,
    );
  }, [client]);

  useEffect(() => {
    void load();
  }, [load]);

  const refresh = useCallback(() => {
    if (settle.current !== null) {
      clearTimeout(settle.current);
    }
    settle.current = setTimeout(() => {
      settle.current = null;
      void load();
    }, SETTLE_MS);
  }, [load]);

  useLiveRefresh(client, refresh);

  useEffect(
    () => () => {
      if (settle.current !== null) {
        clearTimeout(settle.current);
      }
    },
    [],
  );

  if (state.phase === "loading") {
    return (
      <section className="card graph-card placeholder" aria-busy="true">
        <p className="placeholder-kicker">Graph</p>
        <h1 className="placeholder-lead" id="screen-heading">
          Reading the run&rsquo;s graph…
        </h1>
        <p className="placeholder-note">
          The graph is drawn from the same document the Herdr window&rsquo;s graph block shows.
        </p>
      </section>
    );
  }

  if (state.phase === "empty" || state.phase === "error") {
    return (
      <section className="card graph-card placeholder" data-phase={state.phase}>
        <p className="placeholder-kicker">Graph</p>
        <h1 className="placeholder-lead" id="screen-heading">
          {state.phase === "empty" ? state.lead : "The graph could not be read"}
        </h1>
        <p className="placeholder-note">{state.message}</p>
        {state.phase === "error" ? (
          <button type="button" className="graph-retry" onClick={() => void load()}>
            Try again
          </button>
        ) : null}
      </section>
    );
  }

  const { document } = state;
  const layout = layoutGraph(document.tasks);
  const open = document.tasks.find((task) => task.id === selected);

  return (
    <section className="graph-screen" data-device={device}>
      <header className="graph-head">
        <div className="graph-title-block">
          <p className="graph-kicker">Graph</p>
          <h1 className="graph-title" id="screen-heading">
            {document.run.title}
          </h1>
          <p className="graph-sub">
            {count(document.tasks.length, "task")} ·{" "}
            {count(layout.edges.length, "dependency", "dependencies")} ·{" "}
            {document.generated_at === ""
              ? "no timestamp"
              : `written ${when(document.generated_at)}`}
            {state.problem === undefined ? null : (
              <span className="graph-problem"> · {state.problem}</span>
            )}
          </p>
        </div>
        <ul className="graph-legend" aria-label="Legend">
          <li className="tone-amber">
            <span className="graph-dot" aria-hidden="true">
              ●
            </span>{" "}
            Working
          </li>
          <li className="tone-green">
            <span className="graph-dot" aria-hidden="true">
              ●
            </span>{" "}
            Review
          </li>
          <li className="tone-red">
            <span className="graph-dot" aria-hidden="true">
              ●
            </span>{" "}
            Blocked
          </li>
          <li className="tone-neutral">
            <span className="graph-dot" aria-hidden="true">
              ●
            </span>{" "}
            Done
          </li>
          <li className="graph-legend-badges">a2 ◆ verified</li>
        </ul>
      </header>

      <div className="graph-body" data-detail={open !== undefined}>
        <GraphCanvas
          layout={layout}
          selected={selected}
          onSelect={setSelected}
          device={device}
        />
        {open === undefined ? null : (
          <TaskDetail
            task={open}
            document={document}
            device={device}
            onSelect={setSelected}
            onClose={() => setSelected(null)}
          />
        )}
      </div>
    </section>
  );
}

/**
 * Reads the graph, keeping what is already drawn when a refresh fails.
 *
 * The phases are the screen's states: a run that has not started is *empty* with
 * the words the block uses, a run whose document is not written yet is empty
 * with the file's name, and anything else is an error the person can retry. A
 * refresh that fails over a drawing that exists is a line in the header rather
 * than a screen that loses the graph.
 */
async function read(client: ApiClient, previous: GraphState): Promise<GraphState> {
  const keep = (problem: string): GraphState =>
    previous.phase === "ready" ? { ...previous, problem } : { phase: "error", message: problem };
  try {
    const raw = await client.graphRun();
    const parsed = parseRunDocument(raw);
    if (!parsed.ok) {
      return keep(`The run document could not be read: ${parsed.problem}.`);
    }
    return { phase: "ready", document: parsed.document };
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      return empty(client);
    }
    return keep(describe(error));
  }
}

/** What an empty graph says: no run at all, or a run with no document yet. */
async function empty(client: ApiClient): Promise<GraphState> {
  const none = { phase: "empty", lead: "No run to draw yet" } as const;
  try {
    const where = await client.graph();
    if (where.run === undefined || where.run === null) {
      return {
        ...none,
        message: "The graph draws a run's tasks, attempts and gates. `shprd run start` begins one.",
      };
    }
    return {
      phase: "empty",
      lead: "The graph is not written yet",
      message: `The run's document has not been written (${where.run_json}). It is rewritten after the run's next ledger write.`,
    };
  } catch {
    return { ...none, message: "The run's document has not been written yet." };
  }
}

/** One line for a failure a person can act on. */
function describe(error: unknown): string {
  if (error instanceof ApiError) {
    return error.status === 0
      ? "Can't reach shprd. Is the service running?"
      : error.message;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "Something went wrong.";
}

