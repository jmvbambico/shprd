// The Chat and Terminal screens: the picker, and the Chat │ Terminal switch.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { json, scripted, type Answer } from "../settings/testkit";
import { AgentScreen, spaceChoices, type AgentView } from "./AgentScreen";
import type { Screen } from "./screen";

// xterm.js probes a canvas when it loads; these tests draw through a fake screen.
vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));

afterEach(cleanup);

const QUIET: Answer = () => new Promise<Response>(() => undefined);

const fakeScreen = (): Screen => ({ show: () => undefined, focus: () => undefined, dispose: () => undefined });

function snapshot(tasks: Array<{ id: string; title: string }>) {
  return { tasks };
}

function Harness({ client }: { client: ReturnType<typeof scripted>["api"] }) {
  const [view, setView] = useState<AgentView>("chat");
  const [space, setSpace] = useState("coordinator");
  return (
    <AgentScreen client={client} view={view} onView={setView} space={space} onSpace={setSpace} screenFactory={fakeScreen} />
  );
}

describe("spaceChoices", () => {
  it("offers the coordinator, then each task", () => {
    expect(spaceChoices([{ id: "T1", title: "Add a thing" }])).toEqual([
      { id: "coordinator", label: "Assistant" },
      { id: "T1", label: "T1 · Add a thing" },
    ]);
  });

  it("calls the coordinator by the name the person gave it", () => {
    expect(spaceChoices([], "Orchestrator")[0]).toEqual({ id: "coordinator", label: "Orchestrator" });
  });
});

describe("AgentScreen", () => {
  const routes = {
    "GET /api/state": json(200, snapshot([{ id: "T1", title: "Add a thing" }])),
    "GET /api/chat": (call: { url: string }) =>
      json(200, {
        space: call.url.includes("T1") ? "T1" : "coordinator",
        program: "claude",
        session: "s",
        messages: [{ role: "assistant", text: call.url.includes("T1") ? "task words" : "coordinator words", tools: [], at_ms: 1 }],
        cursor: "c",
        more: false,
        note: null,
      }),
    "GET /api/changes": QUIET,
  };

  it("lists the coordinator and the run's tasks, and follows the pick", async () => {
    const { api } = scripted(routes);
    render(<Harness client={api} />);
    await screen.findByText("coordinator words");
    await screen.findByRole("option", { name: "T1 · Add a thing" });
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "T1" } });
    await screen.findByText("task words");
  });

  it("offers the coordinator even when the run has no tasks", async () => {
    const { api } = scripted({ ...routes, "GET /api/state": json(200, snapshot([])) });
    render(<Harness client={api} />);
    expect(screen.getByRole("option", { name: "Assistant" })).toBeDefined();
    await screen.findByText("coordinator words");
  });

  it("switches between Chat and Terminal for the same space", async () => {
    const { api } = scripted(routes);
    render(<Harness client={api} />);
    await screen.findByText("coordinator words");
    expect(screen.getByRole("button", { name: "Chat", pressed: true })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Terminal" }));
    await screen.findByRole("region", { name: "Terminal of coordinator" });
    expect(screen.getByRole("button", { name: "Terminal", pressed: true })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    await screen.findByText("coordinator words");
  });
});
