// Settings is a dialog over the app: opening it keeps the screen under it, and
// closing it goes back there.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { json, reads, scripted } from "../settings/testkit";
import { Shell } from "./Shell";

afterEach(cleanup);

describe("Shell: settings", () => {
  it("opens the dialog over the screen it was on and closes back to it", async () => {
    Object.defineProperty(globalThis, "innerWidth", { value: 1280, configurable: true, writable: true });
    const { api } = scripted(reads({ "GET /api/tunnels": json(200, {}) }));
    render(<Shell client={api} reauth={false} onReauthClose={vi.fn()} onSignedOut={vi.fn()} />);
    expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull();
    fireEvent.click(screen.getAllByRole("button", { name: "Settings" })[0]!);
    expect(screen.getByRole("dialog", { name: "Settings" })).toBeDefined();
    expect(screen.getByRole("heading", { name: "Tasks" })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
    expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Tasks" })).toBeDefined();
  });
});

describe("Shell: tasks", () => {
  const state = {
    seq: 3,
    run: { id: "r1", repo: "/r", base_branch: "main", integration_branch: "integration/m11" },
    tasks: [
      {
        id: "T1",
        title: "Add the board",
        status: "passed",
        derived: { status: "ready" },
        scope: [],
        acceptance: [],
        tags: [],
        skills: [],
        deps: [],
        attempts: [],
        added_at_ms: 0,
        touched_at_ms: 0,
        accepted_failure: false,
      },
    ],
    agents: [],
    needs: [],
    asks: [],
    finished: false,
    spaces: [],
    live_agents: [],
    role_titles: {},
  };
  const routes = () =>
    reads({
      "GET /api/state": json(200, state),
      "GET /api/asks": json(200, { asks: [] }),
      "GET /api/chat": json(200, { space: "T1", program: null, session: null, messages: [], cursor: "", more: false, note: null }),
      "GET /api/tasks/T1/changes": json(200, { task: "T1", base: "b", branch: "f", merge_base: "m", commit_count: 0, commits: [], files: [], added: 0, deleted: 0 }),
      "GET /api/settings": json(200, { values: [], problems: [] }),
    });

  it("opens a task's thread beside the list on a desktop, and its Accept card as a modal", async () => {
    Object.defineProperty(globalThis, "innerWidth", { value: 1280, configurable: true, writable: true });
    const { api } = scripted(routes());
    render(<Shell client={api} reauth={false} onReauthClose={vi.fn()} onSignedOut={vi.fn()} />);
    fireEvent.click((await screen.findAllByText("Add the board"))[0]!);
    expect(await screen.findByRole("tab", { name: "Chat" })).toBeDefined();
    expect(document.querySelector(".thread-sheet")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    const dialog = await screen.findByRole("dialog", { name: /Accept T1\?|T1/ });
    expect(dialog.className).not.toContain("sheet");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog", { name: /T1/ })).toBeNull();
  });

  it("opens a task as a full-screen sheet on a phone, with the Accept card as a sheet", async () => {
    Object.defineProperty(globalThis, "innerWidth", { value: 390, configurable: true, writable: true });
    const { api } = scripted(routes());
    render(<Shell client={api} reauth={false} onReauthClose={vi.fn()} onSignedOut={vi.fn()} />);
    fireEvent.click((await screen.findAllByText("Add the board"))[0]!);
    await screen.findByRole("tab", { name: "Chat" });
    expect(document.querySelector(".thread-sheet")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    const accept = await screen.findByRole("dialog", { name: /T1/ });
    expect(accept.className).toContain("sheet");
  });

  it("shows the chat from the nav, with the coordinator picked", async () => {
    Object.defineProperty(globalThis, "innerWidth", { value: 1280, configurable: true, writable: true });
    const { api } = scripted(
      reads({
        "GET /api/state": json(200, { tasks: [] }),
        "GET /api/chat": json(200, { space: "coordinator", program: null, session: null, messages: [], cursor: "", more: false, note: null }),
      }),
    );
    render(<Shell client={api} reauth={false} onReauthClose={vi.fn()} onSignedOut={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: "Chat" })[0]!);
    expect(screen.getAllByRole("heading", { name: "Chat" }).length).toBeGreaterThan(0);
    expect(document.querySelector(".agent")).not.toBeNull();
    expect(await screen.findByText("No messages yet")).toBeDefined();
  });
});
