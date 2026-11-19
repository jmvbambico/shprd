// The dialog's states and its writes: loading, the service down, data, a refusal
// in the service's own words, the repo scope, the prompts and the phone sheet.

// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SettingsDialog } from "./SettingsDialog";
import { NOTE_MS } from "./TasksPage";
import { PROMPT_MS } from "./PromptsPage";
import { json, prompt, PROMPTS, reads, refusal, ROSTER, scripted, settings, row } from "./testkit";

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
  it("shows one compact row a role: its name, one line, and one model button", async () => {
    open(reads());
    const worker = await screen.findByRole("region", { name: "Worker" });
    const button = within(worker).getByRole("button", { name: "Worker model" });
    expect(button.textContent).toContain("Sonnet");
    expect(within(worker).getByRole("button", { name: "Edit instructions" })).toBeDefined();
    // No stacked Program / Model / Effort rows, and no "nothing sets it" captions.
    expect(screen.queryByText("nothing sets it")).toBeNull();
    expect(screen.queryByText("Program")).toBeNull();
    expect(screen.getByText("reads only")).toBeDefined();
  });

  it("the model button opens the menu on a click, with only the detected programs as tabs", async () => {
    const roster = {
      programs: [
        ...ROSTER.programs,
        { ...ROSTER.programs[1]!, id: "pi", name: "Pi", installed: false },
      ],
    };
    open(reads({ "GET /api/roster": json(200, roster) }));
    const worker = await screen.findByRole("region", { name: "Worker" });
    fireEvent.click(await within(worker).findByRole("button", { name: "Worker model" }));
    const tabs = within(screen.getByRole("tablist", { name: "Programs" })).getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["Claude Code", "Codex"]);
    expect(screen.queryByRole("tab", { name: "Pi" })).toBeNull();
    expect(screen.getByRole("option", { name: /Sonnet/ }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("radiogroup", { name: "Reasoning" })).toBeDefined();
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
    const worker = await screen.findByRole("region", { name: "Worker" });
    fireEvent.click(await within(worker).findByRole("button", { name: "Worker model" }));
    fireEvent.click(await screen.findByRole("option", { name: /opus/ }));
    await waitFor(() => expect(calls.some((call) => call.method === "PUT")).toBe(true));
    const put = calls.find((call) => call.method === "PUT");
    // the level the model does not take is dropped, the model is written
    expect(put?.body).toEqual({ key: "roles.worker.model", value: "opus" });
    await waitFor(() =>
      expect(within(worker).getByRole("button", { name: "Worker model" }).textContent).toContain("opus"),
    );
  });

  it("writes to the repo's entry in This repo, shows a repo badge, and Reset removes the key", async () => {
    const { calls } = open(
      reads({
        "GET /api/settings": json(
          200,
          settings([row("roles.worker.model", "opus", "repo_entry")]),
        ),
        "PUT /api/settings": json(200, { outcome: "removed", settings: settings() }),
      }),
    );
    const worker = await screen.findByRole("region", { name: "Worker" });
    expect(within(worker).getByText("repo")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Reset worker" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Applies to" }));
    fireEvent.click(await screen.findByRole("option", { name: /This repo/ }));
    expect(screen.getByText(/Only this repository \(shprd\)/)).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Reset worker" }));
    await waitFor(() => expect(calls.some((call) => call.method === "PUT")).toBe(true));
    const put = calls.find((call) => call.method === "PUT");
    expect(put?.body).toEqual({ key: "roles.worker.model", for_repo: true });
  });

  it("shows the same-vendor refusal exactly as the API words it, and writes no more of the choice", async () => {
    const words = "the reviewer (codex, openai) and the worker (codex, openai) share a vendor";
    const { calls } = open(
      reads({
        "PUT /api/settings": refusal(409, "refused", words),
      }),
    );
    const worker = await screen.findByRole("region", { name: "Worker" });
    fireEvent.click(await within(worker).findByRole("button", { name: "Worker model" }));
    fireEvent.click(screen.getByRole("tab", { name: "Codex" }));
    fireEvent.change(screen.getByLabelText("Search every agent’s models"), { target: { value: "gpt-6" } });
    fireEvent.click(await screen.findByRole("option", { name: /Use gpt-6/ }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(words);
    expect(alert.textContent).toContain("roles.worker.agent");
    // the program was refused, so the model after it was never sent
    expect(calls.filter((call) => call.method === "PUT")).toHaveLength(1);
  });

  it("puts back what a choice already wrote when a later write of it is refused", async () => {
    let puts = 0;
    const { calls } = open(
      reads({
        "PUT /api/settings": (call) => {
          puts += 1;
          const body = call.body as { key: string };
          // the program goes through, the model after it is refused
          return body.key === "roles.worker.model" && puts === 2
            ? refusal(409, "refused", "that model is not one codex has")
            : json(200, {
                outcome: "stored",
                settings: settings([row("roles.worker.agent", "codex", "you")]),
              });
        },
      }),
    );
    const worker = await screen.findByRole("region", { name: "Worker" });
    fireEvent.click(await within(worker).findByRole("button", { name: "Worker model" }));
    fireEvent.click(screen.getByRole("tab", { name: "Codex" }));
    fireEvent.change(screen.getByLabelText("Search every agent’s models"), { target: { value: "bad-id" } });
    fireEvent.click(await screen.findByRole("option", { name: /Use bad-id/ }));
    await waitFor(() => expect(calls.filter((call) => call.method === "PUT")).toHaveLength(3));
    expect(calls.filter((call) => call.method === "PUT").map((call) => call.body)).toEqual([
      { key: "roles.worker.agent", value: "codex" },
      { key: "roles.worker.model", value: "bad-id" },
      // the program goes back to what the layer held
      { key: "roles.worker.agent", value: "claude" },
    ]);
    expect((await screen.findByRole("alert")).textContent).toContain("that model is not one codex has");
  });

  it("says so when putting a write back is refused too", async () => {
    let puts = 0;
    open(
      reads({
        "PUT /api/settings": (call) => {
          puts += 1;
          const body = call.body as { key: string };
          if (body.key === "roles.worker.model") return refusal(409, "refused", "that model is not one codex has");
          return puts === 3
            ? refusal(409, "refused", "the worker and the reviewer share a vendor")
            : json(200, { outcome: "stored", settings: settings([row("roles.worker.agent", "codex", "you")]) });
        },
      }),
    );
    const worker = await screen.findByRole("region", { name: "Worker" });
    fireEvent.click(await within(worker).findByRole("button", { name: "Worker model" }));
    fireEvent.click(screen.getByRole("tab", { name: "Codex" }));
    fireEvent.change(screen.getByLabelText("Search every agent’s models"), { target: { value: "bad-id" } });
    fireEvent.click(await screen.findByRole("option", { name: /Use bad-id/ }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/back was refused too: the worker and the reviewer share a vendor/));
    expect(screen.getByRole("alert").textContent).toContain("that model is not one codex has");
  });

  it("types an id for a program that lists no models", async () => {
    const { calls } = open(
      reads({
        "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }),
      }),
    );
    const reviewer = await screen.findByRole("region", { name: "Reviewer" });
    fireEvent.click(await within(reviewer).findByRole("button", { name: "Reviewer model" }));
    fireEvent.change(screen.getByLabelText("Search every agent’s models"), { target: { value: "gpt-6-luna" } });
    fireEvent.click(await screen.findByRole("option", { name: /Use gpt-6-luna/ }));
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
    fireEvent.click(screen.getByRole("button", { name: "When a task lands" }));
    fireEvent.click(await screen.findByRole("option", { name: "By itself" }));
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

  it("a reset waits for a save in flight, so the old words never come back", async () => {
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const order: string[] = [];
    open(
      reads({
        "GET /api/prompts/worker": json(200, prompt("worker", "a")),
        "PUT /api/prompts/worker": async () => {
          order.push("put");
          await gate;
          return json(200, prompt("worker", "typed", "edited"));
        },
        "DELETE /api/prompts/worker": () => {
          order.push("delete");
          return json(200, prompt("worker", "shipped"));
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /worker/ }));
    const box = (await screen.findByLabelText("Prompt worker")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "typed" } });
    fireEvent.blur(box);
    await waitFor(() => expect(order).toEqual(["put"]));
    fireEvent.click(screen.getByRole("button", { name: "Reset to the shipped words" }));
    // The reset button is disabled while the save is in flight, so nothing queues.
    release?.();
    await waitFor(() => expect(screen.getByRole("button", { name: "Reset to the shipped words" })).toHaveProperty("disabled", false));
    fireEvent.click(screen.getByRole("button", { name: "Reset to the shipped words" }));
    await waitFor(() => expect(order).toEqual(["put", "delete"]));
    await waitFor(() => expect((screen.getByLabelText("Prompt worker") as HTMLTextAreaElement).value).toBe("shipped"));
    expect(order).toEqual(["put", "delete"]);
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
    // Back saves first; a refusal keeps the words in the box and stays.
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect((await screen.findByRole("alert")).textContent).toContain("a prompt cannot be empty");
    const discard = await screen.findByRole("button", { name: "Discard and go back" });
    expect((screen.getByLabelText("Prompt worker") as HTMLTextAreaElement).value).toBe("");
    fireEvent.click(discard);
    await waitFor(() => expect(screen.queryByLabelText("Prompt worker")).toBeNull());
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
