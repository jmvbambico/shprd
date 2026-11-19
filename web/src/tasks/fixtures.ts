// Fixtures for the tasks screens' tests: a board and the tasks on it. Nothing
// here touches the network or the real HOME.

import type { AskDto, AttemptDto, DerivedStatusDto, SnapshotDto, TaskDto, TaskStatusDto } from "../gen";
import type { Board } from "./useBoard";

/** The stored status each derived word usually goes with. */
const STORED: Record<DerivedStatusDto, TaskStatusDto> = {
  to_do: "queued",
  scheduled: "queued",
  queued: "queued",
  working: "working",
  needs_you: "working",
  ready: "passed",
  merging: "passed",
  done: "integrated",
};

/** One task, with the fields a test does not care about filled in. */
export function task(
  id: string,
  word: DerivedStatusDto,
  extra: Partial<TaskDto> = {},
): TaskDto {
  return {
    id,
    title: `Title of ${id}`,
    status: STORED[word],
    derived: { status: word },
    scope: ["crates/x"],
    acceptance: ["it works"],
    tags: [],
    skills: [],
    deps: [],
    attempts: [],
    last_status: { message: `says ${id}`, seq: 1 },
    ...extra,
  };
}

/** A finished attempt with a hand-in, a check and a review finding. */
export function attemptWithReport(): AttemptDto {
  return {
    n: 1,
    agent: "claude",
    worktree: "/w/x",
    branch: "feature/x",
    report: {
      status: "completed",
      summary: "Added the board.",
      tests: ["cargo test"],
      checks: [
        { gate: "unit", command: "cargo test --workspace", exit_code: 0, output_tail: "ok\ntest result: ok. 9 passed" },
        { gate: "lint", command: "cargo clippy", exit_code: 1, output_tail: "warning: x" },
      ],
      changed_paths: ["a.rs"],
      skills_used: [],
    },
    gates: [
      {
        gate: "review",
        passed: true,
        proven: true,
        findings: [
          { severity: "minor", title: "Name is vague", location: "a.rs:3", detail: "Rename it." },
          { severity: "major", title: "Loses a write", detail: "On a crash." },
        ],
      },
    ],
    reviews: [{ reviewer: "codex", degraded: false }],
  };
}

/** A snapshot of a run with the given tasks. */
export function snapshot(tasks: TaskDto[], extra: Partial<SnapshotDto> = {}): SnapshotDto {
  return {
    seq: 4,
    run: { id: "r1", repo: "/r", base_branch: "main", integration_branch: "integration/m11" },
    tasks,
    agents: [],
    needs: [],
    asks: [],
    finished: false,
    spaces: [],
    live_agents: [],
    role_titles: {},
    ...extra,
  };
}

/** A board a screen can be drawn from, with every function a spy-able stub. */
export function board(state: SnapshotDto | null, extra: Partial<Board> = {}): Board {
  return {
    phase: state === null ? "loading" : "ready",
    state,
    asks: { asks: [] },
    error: null,
    reload: async () => undefined,
    toasts: [],
    dismissed: new Set(),
    dismissToast: () => undefined,
    ...extra,
  };
}

/** One open ask. */
export function ask(task_: string, id: string, extra: Partial<AskDto> = {}): AskDto {
  return {
    id,
    task: task_,
    kind: "question",
    text: "Which branch?",
    options: [],
    created_at_ms: 1000,
    ...extra,
  };
}
