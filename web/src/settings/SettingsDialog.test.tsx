// The dialog's states and its writes: loading, the service down, data, a refusal
// in the service's own words, the repo scope, the prompts and the phone sheet.

// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SettingsDialog } from "./SettingsDialog";
import { NOTE_MS } from "./TasksPage";
import { PROMPT_MS } from "./PromptsPage";
import { json, prompt, PROMPTS, reads, refusal, scripted, settings, row } from "./testkit";

function width(px: number) {
  Object.defineProperty(globalThis, "innerWidth", { value: px, configurable: true, writable: true });
}

beforeEach(() => width(1280));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function open(routes: Parameters<typeof scripted>[0]) {
  const kit = scripted(routes);
  const onClose = vi.fn();
  render(<SettingsDialog client={kit.api} onClose={onClose} onSignedOut={vi.fn()} />);
  return { ...kit, onClose };
}

describe("SettingsDialog: frame", () => {
  it("lists the four pages beside the page and has no Save button", async () => {
    open(reads());
    const nav = screen.getByRole("navigation", { name: "Settings pages" });
    expect(within(nav).getAllByRole("button").map((b) => b.textContent)).toEqual([
      "Agents",
      "Tasks",
      "Prompts",
      "Remote",
    ]);
    await screen.findByText("Worker");
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Settings" }).getAttribute("data-view")).toBe("both");
  });

  it("says it is reading, then says the service is down", async () => {
    open({
      "GET /api/settings": new Promise<Response>(() => undefined) as unknown as Response,
    });
    expect(screen.getByText("Reading the settings…")).toBeDefined();
    cleanup();
    open({
      "GET /api/settings": () => {
        throw new TypeError("connection refused");
      },
      "GET /api/roster": json(200, { programs: [] }),
      "GET /api/prompts": json(200, { prompts: [] }),
    });
    expect(await screen.findByText(/shprd serve is not running|connection refused/)).toBeDefined();
  });

  it("closes on Escape and on the close button", async () => {
    const { onClose } = open(reads());
    await screen.findByText("Worker");
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: "Close settings" }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe("SettingsDialog: Agents", () => {
  it("shows each role's picks with the layer that set them", async () => {
    open(reads());
    const worker = await screen.findByRole("region", { name: "Worker" });
    expect(within(worker).getByText("Claude Code")).toBeDefined();
    expect(within(worker).getByText("sonnet")).toBeDefined();
    expect(within(worker).getAllByText("you").length).toBeGreaterThan(0);
    expect(within(worker).getByText("nothing sets it")).toBeDefined();
    expect(screen.getByText("reads only")).toBeDefined();
  });

  it("writes a model pick at once, to your own settings", async () => {
    const { calls } = open(
      reads({
        "PUT /api/settings": json(200, {
          outcome: "stored",
          settings: settings([row("roles.worker.model", "opus", "you")]),
        }),
      }),
    );
    await screen.findByText("Worker");
    fireEvent.click(screen.getByRole("button", { name: "Change model (roles.worker.model)" }));
    fireEvent.click(await screen.findByRole("option", { name: /opus/ }));
    await screen.findByText("Saved");
    const put = calls.find((call) => call.method === "PUT");
    expect(put?.body).toEqual({ key: "roles.worker.model", value: "opus" });
    expect(await screen.findByText("opus")).toBeDefined();
  });

  it("writes to the repo's entry in the This repo scope, and Reset removes the key", async () => {
    const { calls } = open(
      reads({
        "GET /api/settings": json(
          200,
          settings([row("roles.worker.model", "opus", "repo_entry")]),
        ),
        "PUT /api/settings": json(200, { outcome: "removed", settings: settings() }),
      }),
    );
    await screen.findByText("Worker");
    fireEvent.click(screen.getByRole("button", { name: "This repo" }));
    expect(screen.getByText(/Only this repository \(shprd\)/)).toBeDefined();
    expect(screen.getAllByText(/inherited from/).length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole("button", { name: "Reset roles.worker.model" }));
    await screen.findByText(/Removed: it inherits again/);
    const put = calls.find((call) => call.method === "PUT");
    expect(put?.body).toEqual({ key: "roles.worker.model", for_repo: true });
  });

  it("shows the same-vendor refusal exactly as the API words it", async () => {
    const words = "the reviewer (codex, openai) and the worker (codex, openai) share a vendor";
    open(
      reads({
        "PUT /api/settings": refusal(409, "refused", words),
      }),
    );
    await screen.findByText("Worker");
    fireEvent.click(screen.getByRole("button", { name: "Change program (roles.worker.agent)" }));
    fireEvent.click(await screen.findByRole("option", { name: /Codex/ }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(words);
    expect(alert.textContent).toContain("roles.worker.agent");
  });

  it("types an id for a program that lists no models", async () => {
    const { calls } = open(
      reads({
        "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }),
      }),
    );
    await screen.findByText("Reviewer");
    fireEvent.click(screen.getByRole("button", { name: "Change model (roles.reviewer.model)" }));
    fireEvent.click(await screen.findByRole("button", { name: "Use an id typed" }));
    fireEvent.change(screen.getByLabelText("Type an id"), { target: { value: "gpt-6-luna" } });
    fireEvent.click(screen.getByRole("button", { name: "Use" }));
    await waitFor(() =>
      expect(calls.find((call) => call.method === "PUT")?.body).toEqual({
        key: "roles.reviewer.model",
        value: "gpt-6-luna",
      }),
    );
  });

  it("adds a custom role through roles.order and refuses a bad id without a call", async () => {
    const { calls } = open(
      reads({ "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }) }),
    );
    await screen.findByText("Worker");
    fireEvent.click(screen.getByRole("button", { name: "Add a role" }));
    fireEvent.change(screen.getByLabelText("Role id"), { target: { value: "worker" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/letters, digits/);
    expect(calls.some((call) => call.method === "PUT")).toBe(false);
    fireEvent.change(screen.getByLabelText("Role id"), { target: { value: "docs" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() =>
      expect(calls.find((call) => call.method === "PUT")?.body).toEqual({
        key: "roles.order",
        value: ["coordinator", "worker", "reviewer", "backup", "docs"],
      }),
    );
  });

  it("shows the service's problems and 'not checked' beside the roles", async () => {
    open(
      reads({
        "GET /api/settings": json(
          200,
          settings([], { problems: ["reviewer shares a vendor"], not_checked: "no harness data" }),
        ),
      }),
    );
    expect(await screen.findByText("reviewer shares a vendor")).toBeDefined();
    expect(screen.getByText(/the roles were not checked: no harness data/)).toBeDefined();
  });
});

describe("SettingsDialog: Tasks", () => {
  async function tasks(extra: Record<string, ReturnType<typeof json>> = {}) {
    const kit = open(
      reads({
        "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }),
        ...extra,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Tasks" }));
    await screen.findByText("When a task lands");
    return kit;
  }

  it("writes a choice, a switch and a step as they are made", async () => {
    const { calls } = await tasks();
    fireEvent.click(screen.getByRole("button", { name: "By itself" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(1));
    fireEvent.click(screen.getByRole("switch", { name: "Ask for proof" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(2));
    fireEvent.click(screen.getByRole("button", { name: "More: Review rounds" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(3));
    expect(calls.filter((c) => c.method === "PUT").map((c) => c.body)).toEqual([
      { key: "tasks.merge", value: "auto" },
      { key: "tasks.proof", value: false },
      { key: "tasks.review_rounds", value: 4 },
    ]);
  });

  it("writes a note a moment after typing stops, and a line when it is left", async () => {
    const { calls } = await tasks();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.change(screen.getByLabelText("When a task starts"), { target: { value: "Be kind. Be quick." } });
    expect(calls.some((c) => c.method === "PUT")).toBe(false);
    await act(async () => {
      vi.advanceTimersByTime(NOTE_MS + 10);
    });
    vi.useRealTimers();
    await waitFor(() =>
      expect(calls.find((c) => c.method === "PUT")?.body).toEqual({
        key: "tasks.start_notes",
        value: "Be kind. Be quick.",
      }),
    );
    const line = screen.getByLabelText("Archive branches as");
    fireEvent.focus(line);
    fireEvent.change(line, { target: { value: "archive/" } });
    fireEvent.blur(line);
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(2));
    expect(calls.filter((c) => c.method === "PUT")[1]?.body).toEqual({
      key: "tasks.archive_namespace",
      value: "archive/",
    });
  });
});

describe("SettingsDialog: Prompts", () => {
  it("lists, opens, saves itself and resets", async () => {
    const { calls } = open(
      reads({
        "GET /api/prompts/worker": json(200, prompt("worker", "Be careful.")),
        "PUT /api/prompts/worker": json(200, prompt("worker", "Be very careful.", "edited")),
        "DELETE /api/prompts/worker": json(200, prompt("worker", "Be careful.")),
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Prompts" }));
    expect(await screen.findByText("Edited")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: /worker/ }));
    const box = (await screen.findByLabelText("Prompt worker")) as HTMLTextAreaElement;
    expect(box.value).toBe("Be careful.");
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.change(box, { target: { value: "Be very careful." } });
    await act(async () => {
      vi.advanceTimersByTime(PROMPT_MS + 10);
    });
    vi.useRealTimers();
    await waitFor(() =>
      expect(calls.find((c) => c.method === "PUT")?.body).toEqual({ text: "Be very careful." }),
    );
    expect(await screen.findByText("edited")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Reset to the shipped words" }));
    await waitFor(() => expect((screen.getByLabelText("Prompt worker") as HTMLTextAreaElement).value).toBe("Be careful."));
    expect(calls.some((c) => c.method === "DELETE")).toBe(true);
  });

  it("keeps the words and shows the refusal when a save is refused", async () => {
    open(
      reads({
        "GET /api/prompts/worker": json(200, prompt("worker", "x")),
        "PUT /api/prompts/worker": refusal(400, "bad_request", "a prompt cannot be empty"),
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /worker/ }));
    const box = (await screen.findByLabelText("Prompt worker")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "" } });
    fireEvent.blur(box);
    expect((await screen.findByRole("alert")).textContent).toContain("a prompt cannot be empty");
    expect(box.value).toBe("");
  });

  it("writes words typed while a save was in flight, even when leaving at once", async () => {
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const bodies: unknown[] = [];
    const { calls } = open(
      reads({
        "GET /api/prompts/worker": json(200, prompt("worker", "a")),
        "PUT /api/prompts/worker": async (call) => {
          bodies.push(call.body);
          if (bodies.length === 1) {
            await gate;
          }
          return json(200, prompt("worker", "stored", "edited"));
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /worker/ }));
    const box = (await screen.findByLabelText("Prompt worker")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "draft A" } });
    fireEvent.blur(box);
    await waitFor(() => expect(bodies).toHaveLength(1));
    fireEvent.change(box, { target: { value: "draft B" } });
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    release?.();
    await waitFor(() => expect(bodies).toEqual([{ text: "draft A" }, { text: "draft B" }]));
    void calls;
  });

  it("says when there are no prompts", async () => {
    open(reads({ "GET /api/prompts": json(200, { prompts: [] }) }));
    fireEvent.click(screen.getByRole("button", { name: "Prompts" }));
    expect(await screen.findByText("No prompts yet.")).toBeDefined();
    void PROMPTS;
  });
});

describe("SettingsDialog: on a phone", () => {
  it("opens on the page list, then a page, and back", async () => {
    width(390);
    open(reads());
    const dialog = screen.getByRole("dialog", { name: "Settings" });
    expect(dialog.getAttribute("data-view")).toBe("list");
    expect(screen.queryByText("Worker")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Agents/ }));
    expect(dialog.getAttribute("data-view")).toBe("page");
    expect(await screen.findByText("Worker")).toBeDefined();
    expect(screen.queryByRole("navigation", { name: "Settings pages" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /‹ Settings/ }));
    expect(dialog.getAttribute("data-view")).toBe("list");
  });
});
