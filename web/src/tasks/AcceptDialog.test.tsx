// The Accept card's states and verbs: loading, a task that left, data, a
// settled task, the three verbs and the fresh-password path.

// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { json, refusal, scripted, type Answer } from "../settings/testkit";
import { AcceptDialog } from "./AcceptDialog";
import { attemptWithReport, board, snapshot, task } from "./fixtures";

afterEach(cleanup);

const changes = {
  task: "T1",
  base: "integration/m11",
  branch: "feature/t1",
  merge_base: "abc",
  commit_count: 2,
  commits: [{ sha: "abc1234", subject: "feat: the thing" }],
  files: [{ path: "src/a.rs", added: 12, deleted: 4, binary: false }],
  added: 12,
  deleted: 4,
};

const ready = task("T1", "ready", { attempts: [attemptWithReport()] });

function routes(extra: Record<string, Answer> = {}) {
  return {
    "GET /api/tasks/T1/changes": json(200, changes),
    "GET /api/settings": json(200, { values: [{ key: "tasks.merge", value: "ask", source: "built_in" }], problems: [] }),
    ...extra,
  };
}

describe("AcceptDialog", () => {
  it("shows the summary, changes, checks and the reviewer's findings", async () => {
    const { api } = scripted(routes());
    render(
      <AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />,
    );
    expect(screen.getByRole("heading", { name: "Accept T1?" })).toBeDefined();
    expect(screen.getByText("Added the board.")).toBeDefined();
    await waitFor(() => expect(screen.getByText("1 file changed")).toBeDefined());
    expect(screen.getByText("2 commits ·")).toBeDefined();
    expect(screen.getByText("src/a.rs")).toBeDefined();
    expect(screen.getByText("cargo test --workspace")).toBeDefined();
    expect(screen.getByText("Failed")).toBeDefined();
    expect(screen.getByText("codex reviewed it: passed · another vendor")).toBeDefined();
    expect(screen.getByText("Loses a write")).toBeDefined();
  });

  it("is a modal on a desktop and a sheet on a phone", () => {
    const { api } = scripted(routes());
    const view = render(
      <AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="phone" reauthTick={0} onClose={vi.fn()} />,
    );
    expect(screen.getByRole("dialog").className).toContain("sheet");
    view.unmount();
    render(
      <AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />,
    );
    expect(screen.getByRole("dialog").className).not.toContain("sheet");
  });

  it("says so when the task is no longer on the board, and when the branch cannot be read", async () => {
    const { api } = scripted(routes({ "GET /api/tasks/T1/changes": refusal(500, "internal", "git said no") }));
    render(<AcceptDialog client={api} board={board(snapshot([]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />);
    expect(screen.getByText("This task is no longer on the board.")).toBeDefined();
    cleanup();
    render(<AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("git said no")).toBeDefined());
  });

  it("a settled task offers only Close and says where it is", () => {
    const { api } = scripted(routes());
    render(
      <AcceptDialog client={api} board={board(snapshot([task("T1", "done")]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />,
    );
    expect(screen.getByText("Landed: its work is in the base branch.")).toBeDefined();
    expect(screen.getByRole("button", { name: "Close" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Accept" })).toBeNull();
  });

  it("Accept posts task.accept, reads the board again and closes", async () => {
    const { api, calls } = scripted(routes({ "POST /api/action": json(200, { ok: true, seq: 9 }) }));
    const onClose = vi.fn();
    const reload = vi.fn(async () => undefined);
    render(
      <AcceptDialog client={api} board={board(snapshot([ready]), { reload })} taskId="T1" device="desktop" reauthTick={0} onClose={onClose} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(reload).toHaveBeenCalled();
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({ kind: "task.accept", task: "T1" });
  });

  it("hides Accept where tasks.merge lands work without one, and says so", async () => {
    const { api } = scripted(
      routes({ "GET /api/settings": json(200, { values: [{ key: "tasks.merge", value: "auto", source: "you" }], problems: [] }) }),
    );
    render(<AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("tasks.merge is auto: T1 lands without an approval")).toBeDefined());
    expect(screen.queryByRole("button", { name: "Accept" })).toBeNull();
  });

  it("Drop asks for a reason first and sends it", async () => {
    const { api, calls } = scripted(routes({ "POST /api/action": json(200, { ok: true, seq: 9 }) }));
    const onClose = vi.fn();
    render(
      <AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={onClose} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Drop task" }));
    const drop = screen.getByRole("button", { name: "Drop task" });
    expect((drop as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Why drop it"), { target: { value: "not needed" } });
    fireEvent.click(drop);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({ kind: "task.drop", task: "T1", reason: "not needed" });
  });

  it("Send back is for a failed attempt", () => {
    const { api } = scripted(routes());
    render(
      <AcceptDialog client={api} board={board(snapshot([task("T1", "queued", { status: "failed" })]))} taskId="T1" device="desktop" reauthTick={0} onClose={vi.fn()} />,
    );
    expect(screen.getByRole("button", { name: "Send back" })).toBeDefined();
  });

  it("a refusal stays on the card in the service's words, and nothing closes", async () => {
    const { api } = scripted(routes({ "POST /api/action": refusal(409, "conflict", "the review has not passed") }));
    const onClose = vi.fn();
    render(<AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("the review has not passed"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("asks for the password on {code: reauth} and sends the verb again once it is given", async () => {
    let attempts = 0;
    const { api, calls } = scripted(
      routes({
        "POST /api/action": () => {
          attempts += 1;
          return attempts === 1 ? refusal(401, "reauth", "enter your password again") : json(200, { ok: true, seq: 9 });
        },
      }),
    );
    const onReauth = vi.fn();
    api.onReauth(onReauth);
    const onClose = vi.fn();
    const props = { client: api, board: board(snapshot([ready])), taskId: "T1", device: "desktop" as const, onClose };
    const view = render(<AcceptDialog {...props} reauthTick={0} />);
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(onReauth).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Enter your password"));
    expect(onClose).not.toHaveBeenCalled();
    // The app's dialog took the password: the tick goes up and the verb goes again.
    await act(async () => {
      view.rerender(<AcceptDialog {...props} reauthTick={1} />);
    });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(calls.filter((c) => c.method === "POST")).toHaveLength(2);
  });

  it("Esc and × close it without doing anything", () => {
    const { api, calls } = scripted(routes());
    const onClose = vi.fn();
    render(<AcceptDialog client={api} board={board(snapshot([ready]))} taskId="T1" device="desktop" reauthTick={0} onClose={onClose} />);
    fireEvent.keyDown(globalThis as unknown as Element, { key: "Escape" });
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Close card" }));
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(calls.some((c) => c.method === "POST")).toBe(false);
  });
});
