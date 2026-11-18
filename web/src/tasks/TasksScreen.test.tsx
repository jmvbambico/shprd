// The Tasks screen's states — loading, empty, error, data — and the paths from a
// tap to the service: add a task, start one, search, filter, fold, open.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { json, scripted } from "../settings/testkit";
import { board, snapshot, task } from "./fixtures";
import { TasksScreen } from "./TasksScreen";

beforeEach(() => {
  try {
    globalThis.localStorage?.clear();
  } catch {
    // jsdom may not give storage; the screen works the same.
  }
});
afterEach(cleanup);

const none = () => scripted({}).api;

describe("TasksScreen states", () => {
  it("says it is reading while there is no answer yet", () => {
    render(<TasksScreen client={none()} board={board(null)} onOpen={vi.fn()} onReview={vi.fn()} />);
    expect(screen.getByText("Reading the board…")).toBeDefined();
  });

  it("says why, and offers another try, when the service never answered", () => {
    const reload = vi.fn(async () => undefined);
    render(
      <TasksScreen
        client={none()}
        board={board(null, { phase: "error", error: "Can't reach shprd. Is the service running?", reload })}
        onOpen={vi.fn()}
        onReview={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert").textContent).toContain("Can't reach shprd");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(reload).toHaveBeenCalled();
  });

  it("an empty run says how to add the first task", () => {
    render(<TasksScreen client={none()} board={board(snapshot([]))} onOpen={vi.fn()} onReview={vi.fn()} />);
    expect(screen.getByText("No tasks yet.")).toBeDefined();
  });
});

describe("TasksScreen with data", () => {
  const state = snapshot([
    task("T1", "needs_you", { derived: { status: "needs_you", need: "asks" } }),
    task("T2", "ready"),
    task("T3", "working"),
    task("T4", "to_do"),
    task("T5", "done"),
  ]);

  it("groups by derived status with For you first and Done as one line", () => {
    render(<TasksScreen client={none()} board={board(state)} onOpen={vi.fn()} onReview={vi.fn()} />);
    const you = screen.getByRole("button", { name: /For you/ });
    expect(you.textContent).toContain("2");
    expect(screen.getByRole("button", { name: /1 done/ })).toBeDefined();
    expect(screen.queryByText("Title of T5")).toBeNull();
    // the count badges
    expect(screen.getByLabelText("1 need you")).toBeDefined();
    expect(screen.getByLabelText("1 ready")).toBeDefined();
  });

  it("opens a task's thread from its row, and its Accept card from the Review pill", () => {
    const onOpen = vi.fn();
    const onReview = vi.fn();
    render(<TasksScreen client={none()} board={board(state)} onOpen={onOpen} onReview={onReview} />);
    fireEvent.click(screen.getByText("Title of T3"));
    expect(onOpen).toHaveBeenCalledWith("T3");
    fireEvent.click(screen.getByRole("button", { name: "Review T2" }));
    expect(onReview).toHaveBeenCalledWith("T2");
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("filters as you type and says when nothing matches", () => {
    render(<TasksScreen client={none()} board={board(state)} onOpen={vi.fn()} onReview={vi.fn()} />);
    const box = screen.getByLabelText("Filter tasks");
    fireEvent.change(box, { target: { value: "t4" } });
    expect(screen.getByText("Title of T4")).toBeDefined();
    expect(screen.queryByText("Title of T3")).toBeNull();
    fireEvent.change(box, { target: { value: "zzz" } });
    expect(screen.getByText("No tasks match “zzz”.")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "× Clear" }));
    expect(screen.getByText("Title of T3")).toBeDefined();
  });

  it("a chip shows one status flat, and More holds the rest", () => {
    render(<TasksScreen client={none()} board={board(state)} onOpen={vi.fn()} onReview={vi.fn()} />);
    fireEvent.click(within(screen.getByRole("group", { name: "Filter by status" })).getByRole("button", { name: /^Working/ }));
    expect(screen.getByText("Title of T3")).toBeDefined();
    expect(screen.queryByText("Title of T4")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "More ⌄" }));
    const more = screen.getByRole("list", { name: "More statuses" });
    fireEvent.click(within(more).getByRole("button", { name: /To do/ }));
    expect(screen.getByText("Title of T4")).toBeDefined();
  });

  it("folds a heading, and opens Done", () => {
    render(<TasksScreen client={none()} board={board(state)} onOpen={vi.fn()} onReview={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: /For you/ }));
    expect(screen.queryByText("Title of T1")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /1 done/ }));
    expect(screen.getByText("Title of T5")).toBeDefined();
  });
});

describe("TasksScreen verbs", () => {
  it("Start posts the engine's verb and reads the board again", async () => {
    const { api, calls } = scripted({ "POST /api/action": json(200, { ok: true, seq: 5 }) });
    const reload = vi.fn(async () => undefined);
    render(
      <TasksScreen client={api} board={board(snapshot([task("T4", "to_do")]), { reload })} onOpen={vi.fn()} onReview={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Start T4" }));
    await waitFor(() => expect(reload).toHaveBeenCalled());
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({ kind: "task.start", task: "T4" });
  });

  it("a refusal is drawn where the person pressed", async () => {
    const { api } = scripted({
      "POST /api/action": json(409, { code: "conflict", message: "T4 is already started" }),
    });
    render(<TasksScreen client={api} board={board(snapshot([task("T4", "to_do")]))} onOpen={vi.fn()} onReview={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Start T4" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("T4 is already started"));
  });
});

describe("TasksScreen add a task", () => {
  it("says why it cannot be sent yet, then posts to /api/tasks", async () => {
    const { api, calls } = scripted({ "POST /api/tasks": json(200, { ok: true, seq: 6 }) });
    const reload = vi.fn(async () => undefined);
    render(<TasksScreen client={api} board={board(snapshot([]), { reload })} onOpen={vi.fn()} onReview={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Add a task" }));
    const form = screen.getByRole("form", { name: "New task" });
    expect(within(form).getByText("A task needs a title")).toBeDefined();
    fireEvent.change(within(form).getByLabelText("Title"), { target: { value: "Do it" } });
    fireEvent.change(within(form).getByLabelText("Paths it may change"), { target: { value: "crates/a, web" } });
    fireEvent.change(within(form).getByLabelText("What must be true when it is done"), { target: { value: "tests pass" } });
    fireEvent.click(within(form).getByRole("button", { name: "Add task" }));
    await waitFor(() => expect(reload).toHaveBeenCalled());
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({
      title: "Do it",
      scope: ["crates/a", "web"],
      acceptance: ["tests pass"],
      tags: [],
      skills: [],
      deps: [],
    });
    expect(screen.queryByRole("form", { name: "New task" })).toBeNull();
  });

  it("keeps the form and says what the service said when it refuses", async () => {
    const { api } = scripted({ "POST /api/tasks": json(400, { code: "bad_request", message: "scope escapes the repo" }) });
    render(<TasksScreen client={api} board={board(snapshot([]))} onOpen={vi.fn()} onReview={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Add a task" }));
    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "x" } });
    fireEvent.change(screen.getByLabelText("Paths it may change"), { target: { value: "../out" } });
    fireEvent.change(screen.getByLabelText("What must be true when it is done"), { target: { value: "y" } });
    fireEvent.click(screen.getByRole("button", { name: "Add task" }));
    await waitFor(() => expect(screen.getByText("scope escapes the repo")).toBeDefined());
    expect(screen.getByRole("form", { name: "New task" })).toBeDefined();
  });
});
