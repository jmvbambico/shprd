// The task thread: its head and switch, the chat with a question card in the
// timeline (answered through /api/asks/answer), the terminal's pointer and the
// pull request.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { json, refusal, scripted, type Answer } from "../settings/testkit";
import { ask, attemptWithReport, board, snapshot, task } from "./fixtures";
import { TaskThread } from "./TaskThread";

afterEach(cleanup);

const chatPage = {
  space: "T1",
  program: "claude",
  session: "s1",
  messages: [
    { role: "user", text: "Start with the route", tools: [], at_ms: 1000 },
    { role: "assistant", text: "On it.", tools: [{ name: "Bash", summary: "cargo test" }], at_ms: 2000 },
  ],
  cursor: "c2",
  more: false,
  note: null,
};

function routes(extra: Record<string, Answer> = {}) {
  return { "GET /api/chat": json(200, chatPage), ...extra };
}

function open(tasks = [task("T1", "needs_you")], extra = {}, routeExtra: Record<string, Answer> = {}) {
  const s = scripted(routes(routeExtra));
  const props = {
    client: s.api,
    board: board(snapshot(tasks), extra),
    taskId: "T1",
    onBack: vi.fn(),
    onReview: vi.fn(),
  };
  render(<TaskThread {...props} />);
  return { ...s, props };
}

describe("TaskThread", () => {
  it("opens on the chat, with the agent's conversation and the head's words", async () => {
    open([task("T1", "working", { last_status: { message: "wiring", progress: 40, seq: 2 } })]);
    expect(screen.getByText("wiring · 40%")).toBeDefined();
    expect(screen.getByRole("tab", { name: "Chat" }).getAttribute("aria-selected")).toBe("true");
    await waitFor(() => expect(screen.getByText("On it.")).toBeDefined());
    expect(screen.getByText("Start with the route")).toBeDefined();
    expect(screen.getByText("→ Bash · cargo test")).toBeDefined();
    expect(screen.getByText("Start of conversation")).toBeDefined();
  });

  it("says when there is nothing yet, and when the chat cannot be read", async () => {
    const quiet = { ...chatPage, messages: [], note: "claude has not written a session yet" };
    open([task("T1", "working")], {}, { "GET /api/chat": json(200, quiet) });
    await waitFor(() => expect(screen.getByText("claude has not written a session yet")).toBeDefined());
    cleanup();
    open([task("T1", "working")], {}, { "GET /api/chat": refusal(500, "internal", "transcript unreadable") });
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("transcript unreadable"));
  });

  it("Review shows only for work that is ready, and opens the card", () => {
    const { props } = open([task("T1", "ready", { attempts: [attemptWithReport()] })]);
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(props.onReview).toHaveBeenCalledWith("T1");
    fireEvent.click(screen.getByRole("button", { name: "‹ Tasks" }));
    expect(props.onBack).toHaveBeenCalled();
    cleanup();
    open([task("T1", "working")]);
    expect(screen.queryByRole("button", { name: "Review" })).toBeNull();
  });

  it("puts the question card in the timeline and answers with a choice", async () => {
    const { calls } = open(
      [task("T1", "needs_you")],
      { asks: { asks: [ask("T1", "q1", { kind: "permission", text: "Run the tests?", options: ["yes", "no"] })] } },
      { "POST /api/asks/answer": json(200, { ok: true, ask: ask("T1", "q1") }) },
    );
    await waitFor(() => expect(screen.getByText("Run the tests?")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: "yes" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/api/asks/answer")).toBe(true));
    expect(calls.find((c) => c.url === "/api/asks/answer")?.body).toEqual({ task: "T1", ask: "q1", answer: "yes" });
  });

  it("a question takes typed words, and a refused answer is said where it was given", async () => {
    open(
      [task("T1", "needs_you")],
      { asks: { asks: [ask("T1", "q1")] } },
      { "POST /api/asks/answer": refusal(409, "conflict", "that ask was already answered") },
    );
    await waitFor(() => expect(screen.getByText("Which branch?")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: "Type an answer" }));
    fireEvent.change(screen.getByLabelText("Your answer"), { target: { value: "main" } });
    const form = screen.getByLabelText("Your answer").closest("form") as HTMLElement;
    fireEvent.click(within(form).getByRole("button", { name: "Send" }));
    await waitFor(() => expect(screen.getByText("that ask was already answered")).toBeDefined());
  });

  it("an escalation names the verb to run and offers no button for it", async () => {
    open([task("T1", "needs_you")], {
      state: snapshot([task("T1", "needs_you")], {
        needs: [{ task: "T1", body: "The review spent: run `shprd retry T1`", default_action: "wait", seq: 4, kind: "decision", actions: [], key: "", for_you: true }],
      }),
    });
    await waitFor(() => expect(screen.getByText("shprd retry T1")).toBeDefined());
    expect(screen.getByText("wait")).toBeDefined();
  });

  it("sends a turn from the composer and never an empty one", async () => {
    const { calls } = open([task("T1", "working")], {}, { "POST /api/chat": json(200, { ok: true }) });
    await waitFor(() => expect(screen.getByText("On it.")).toBeDefined());
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Message the agent"), { target: { value: " go " } });
    fireEvent.click(send);
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({ space: "T1", text: "go" });
  });

  it("the terminal says to open the desktop; the pull request shows once there is one", async () => {
    const withPr = task("T1", "ready", { pr: { number: 7, url: "https://github.com/o/r/pull/7", head: "feature/t1", state: "open", approved: false } });
    open([withPr], {}, {
      "GET /api/tasks/T1/changes": json(200, {
        task: "T1", base: "integration/m11", branch: "feature/t1", merge_base: "a", commit_count: 1, commits: [],
        files: [{ path: "a", added: 3, deleted: 1, binary: false }], added: 3, deleted: 1,
      }),
    });
    fireEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(screen.getByText(/Open the task’s space on the Herdr desktop/)).toBeDefined();
    fireEvent.click(screen.getByRole("tab", { name: "Pull request" }));
    expect(screen.getByText("#7")).toBeDefined();
    expect(screen.getByText("Waiting for an approval")).toBeDefined();
    await waitFor(() => expect(screen.getByText("+3")).toBeDefined());
    expect(screen.getByRole("link", { name: "Open on GitHub" }).getAttribute("href")).toBe("https://github.com/o/r/pull/7");
  });

  it("has no Pull request tab until the task has one", () => {
    open([task("T1", "working")]);
    expect(screen.queryByRole("tab", { name: "Pull request" })).toBeNull();
  });

  it("says when the task left the board", () => {
    open([]);
    expect(screen.getByText("This task is no longer on the board.")).toBeDefined();
  });
});
