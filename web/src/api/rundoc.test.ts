// The document reader: the graph draws whatever it returns, so these pin the
// two things it must never do — draw a task the document did not state, and
// refuse a run over a field no screen shows.

import { describe, expect, it } from "vitest";

import { criteria, evidenceMark, latestAttempt, parseRunDocument, taskTone } from "./rundoc";

/** A document with the spine the reader needs. */
function document(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    dagr: 3,
    run: { id: "r1", title: "shprd · r1", started_at: "2026-11-16T10:00:00Z" },
    generated_at: "2026-11-16T10:05:00Z",
    projects: [{ id: "crates", title: "crates" }],
    tasks: [
      {
        id: "T1",
        title: "one",
        kind: "impl",
        state: "working",
        deps: [],
        criteria: "it works; it is small",
        attempts: [
          {
            id: "T1·a1",
            n: 1,
            cause: { type: "initial" },
            actor: "pi",
            model: "opus",
            state: "done",
            started_at: "2026-11-16T10:01:00Z",
            ended_at: "2026-11-16T10:02:00Z",
            outcome: { result: "done", evidence: "verified" },
          },
        ],
      },
    ],
    events: [],
    ...overrides,
  };
}

describe("parseRunDocument", () => {
  it("reads the document the projector writes", () => {
    const result = parseRunDocument(document());
    expect(result.ok).toBe(true);
    if (!result.ok) {
      return;
    }
    expect(result.document.run).toEqual({
      id: "r1",
      title: "shprd · r1",
      started_at: "2026-11-16T10:00:00Z",
    });
    expect(result.document.tasks).toHaveLength(1);
    const [task] = result.document.tasks;
    expect(task?.state).toBe("working");
    expect(task?.criteria).toBe("it works; it is small");
    expect(task?.attempts[0]?.outcome?.evidence).toBe("verified");
    expect(result.document.projects).toEqual([{ id: "crates", title: "crates" }]);
  });

  it("refuses an answer that is not a document, naming the field", () => {
    for (const [value, why] of [
      [null, "did not answer with a document"],
      [[], "did not answer with a document"],
      [{ tasks: [] }, "no run identity"],
      [document({ run: { title: "no id" } }), "no run identity"],
      [document({ tasks: "one" }), "no task list"],
      [document({ tasks: [{ title: "no id" }] }), "task 1 has no id"],
      [document({ tasks: [{ id: "T1" }] }), "has no title"],
      [document({ tasks: [{ id: "T1", title: "t" }] }), "has no state"],
    ] as const) {
      const result = parseRunDocument(value);
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.problem).toContain(why);
      }
    }
  });

  it("keeps a state this build has never heard of", () => {
    // dagr's vocabulary is open; a newer producer must still draw.
    const result = parseRunDocument(
      document({ tasks: [{ id: "T9", title: "nine", state: "someday" }] }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.document.tasks[0]?.state).toBe("someday");
      expect(taskTone("someday")).toEqual({ word: "someday", tone: "neutral" });
    }
  });

  it("keeps a document whose optional fields are missing", () => {
    const result = parseRunDocument(
      document({
        projects: undefined,
        events: undefined,
        tasks: [{ id: "T1", title: "one", state: "queued" }],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.document.projects).toEqual([]);
      expect(result.document.events).toEqual([]);
      expect(result.document.tasks[0]?.deps).toEqual([]);
      expect(result.document.tasks[0]?.attempts).toEqual([]);
    }
  });

  it("orders attempts by their number, and drops one with no identity", () => {
    const attempt = (n: number, id: string) => ({
      id,
      n,
      cause: { type: "initial" },
      actor: "pi",
      state: "done",
      started_at: "2026-11-16T10:00:00Z",
    });
    const result = parseRunDocument(
      document({
        tasks: [
          {
            id: "T1",
            title: "one",
            state: "review",
            attempts: [attempt(2, "T1·a2"), attempt(1, "T1·a1"), { n: 3 }],
          },
        ],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.document.tasks[0]?.attempts.map((entry) => entry.id)).toEqual([
        "T1·a1",
        "T1·a2",
      ]);
    }
  });
});

describe("taskTone", () => {
  it("names every state the projection writes", () => {
    expect(taskTone("queued")).toEqual({ word: "Queued", tone: "neutral" });
    expect(taskTone("working")).toEqual({ word: "Working", tone: "amber" });
    expect(taskTone("review")).toEqual({ word: "Review", tone: "green" });
    expect(taskTone("blocked")).toEqual({ word: "Blocked", tone: "red" });
    expect(taskTone("done")).toEqual({ word: "Done", tone: "neutral" });
  });

  it("falls back to the word itself for one it does not know", () => {
    expect(taskTone("")).toEqual({ word: "Unknown", tone: "neutral" });
  });
});

describe("evidenceMark", () => {
  it("uses the contract's glyphs", () => {
    expect(evidenceMark("verified").glyph).toBe("◆");
    expect(evidenceMark("reported").glyph).toBe("◇");
    expect(evidenceMark("heuristic").glyph).toBe("≈");
    expect(evidenceMark("asserted").glyph).toBe("!");
  });

  it("says so when there is no evidence word", () => {
    expect(evidenceMark("").word).toBe("no evidence");
  });
});

describe("a task's derived words", () => {
  it("takes the latest attempt", () => {
    const result = parseRunDocument(document());
    expect(result.ok).toBe(true);
    if (result.ok) {
      const task = result.document.tasks[0];
      expect(task).toBeDefined();
      if (task !== undefined) {
        expect(latestAttempt(task)?.n).toBe(1);
      }
    }
  });

  it("splits the criteria the way dagr joins them", () => {
    const result = parseRunDocument(document());
    expect(result.ok).toBe(true);
    if (result.ok) {
      const task = result.document.tasks[0];
      expect(task === undefined ? [] : criteria(task)).toEqual(["it works", "it is small"]);
    }
  });
});
