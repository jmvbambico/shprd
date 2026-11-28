// A role's name: the box on the Agents page, what it writes, and the coordinator's
// restart link.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { cleanTitle, needsCoordinatorRestart, roleTitle, TITLE_MAX } from "../roles";
import { SettingsDialog } from "./SettingsDialog";
import { roles, type Role } from "./model";
import { json, reads, refusal, row, scripted, settings } from "./testkit";

afterEach(cleanup);

const named = settings([
  row("roles.coordinator.title", "Orchestrator", "you"),
  row("roles.coordinator.kind", "coordinator"),
  row("roles.worker.title", "Dev", "you"),
]);

describe("role names", () => {
  it("falls back from the person's name to the built-in word to the key", () => {
    expect(roleTitle({ worker: "Dev" }, "worker")).toBe("Dev");
    expect(roleTitle({ worker: "   " }, "worker")).toBe("Worker");
    expect(roleTitle(null, "coordinator")).toBe("Assistant");
    expect(roleTitle(undefined, "docs")).toBe("docs");
  });

  it("trims, joins and cuts a typed name", () => {
    expect(cleanTitle("  Dev   team ")).toBe("Dev team");
    expect(cleanTitle("   ")).toBe("");
    expect(cleanTitle("x".repeat(80))).toHaveLength(TITLE_MAX);
  });

  it("only the coordinator's starting keys need a restart", () => {
    expect(needsCoordinatorRestart("roles.coordinator.model")).toBe(true);
    expect(needsCoordinatorRestart("roles.worker.model")).toBe(false);
    expect(needsCoordinatorRestart("roles.coordinator.title")).toBe(false);
  });

  it("the Agents model reads each role's name and its fallback", () => {
    const [coordinator, worker, reviewer] = roles(named, null) as [Role, Role, Role];
    expect(coordinator.title).toBe("Orchestrator");
    expect(coordinator.name).toMatchObject({ key: "roles.coordinator.title", value: "Orchestrator", fallback: "Assistant" });
    expect(worker.title).toBe("Dev");
    expect(reviewer.title).toBe("Reviewer (s)");
    expect(reviewer.name.value).toBe("Reviewer");
  });
});

describe("the inline name", () => {
  async function dialog(extra: Record<string, ReturnType<typeof json> | Response> = {}) {
    const kit = scripted(
      reads({ "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }), ...extra }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    return kit;
  }

  it("a click on the name opens a field; Enter writes the trimmed name", async () => {
    const kit = await dialog();
    const worker = await screen.findByRole("region", { name: "Worker (s)" });
    expect(within(worker).queryByLabelText("Worker (s) name")).toBeNull();
    fireEvent.click(within(worker).getByRole("button", { name: "Rename Worker (s)" }));
    const box = within(worker).getByLabelText("Worker (s) name");
    fireEvent.change(box, { target: { value: "  Dev " } });
    fireEvent.keyDown(box, { key: "Enter" });
    fireEvent.blur(box);
    await waitFor(() =>
      expect(kit.calls.find((call) => call.method === "PUT")?.body).toEqual({
        key: "roles.worker.title",
        value: "Dev",
      }),
    );
  });

  it("Esc leaves the field, writes nothing and does not close the window", async () => {
    const kit = await dialog();
    const worker = await screen.findByRole("region", { name: "Worker (s)" });
    fireEvent.click(within(worker).getByRole("button", { name: "Rename Worker (s)" }));
    const box = within(worker).getByLabelText("Worker (s) name");
    fireEvent.change(box, { target: { value: "Nope" } });
    fireEvent.keyDown(box, { key: "Escape" });
    expect(within(worker).queryByLabelText("Worker (s) name")).toBeNull();
    expect(kit.calls.some((call) => call.method === "PUT")).toBe(false);
  });

  it("a blank name goes back to the built-in one", async () => {
    const kit = await dialog({
      "GET /api/settings": json(200, named),
      "PUT /api/settings": json(200, { outcome: "removed", settings: named }),
    });
    const worker = await screen.findByRole("region", { name: "Dev" });
    fireEvent.click(within(worker).getByRole("button", { name: "Rename Dev" }));
    const box = within(worker).getByLabelText("Dev name");
    fireEvent.change(box, { target: { value: "  " } });
    fireEvent.blur(box);
    await waitFor(() =>
      expect(kit.calls.find((call) => call.method === "PUT")?.body).toEqual({ key: "roles.worker.title" }),
    );
  });
});

describe("the restart notice", () => {
  const live = { live_agents: [{ id: "a1", space: "s", task: null, role: "coordinator", program: "claude", state: "working", terminal: "t1" }] };

  async function dialog(state: unknown, restart: Response = new Response(null, { status: 204 })) {
    const kit = scripted(
      reads({
        "GET /api/settings": json(200, named),
        "GET /api/state": json(200, state),
        "PUT /api/settings": json(200, { outcome: "stored", settings: named }),
        "POST /api/coordinator/restart": restart,
      }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    return kit;
  }

  async function changeModel(block: HTMLElement) {
    fireEvent.click(within(block).getByRole("button", { name: "Orchestrator model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Sonnet/ }));
  }

  it("is not a permanent row: nothing shows until the coordinator's choice changed", async () => {
    await dialog(live);
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    expect(within(block).queryByText("Restart the coordinator now")).toBeNull();
    expect(screen.queryByText("Restart the coordinator")).toBeNull();
  });

  it("shows after the coordinator's model changed while it runs, and restarts once", async () => {
    const kit = await dialog(live);
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    await changeModel(block);
    const button = await within(block).findByRole("button", { name: "Restart the coordinator now" });
    // A worker's change shows no notice on the worker.
    expect(within(screen.getByRole("region", { name: "Dev" })).queryByText("Restart the coordinator now")).toBeNull();
    fireEvent.click(button);
    await waitFor(() =>
      expect(kit.calls.filter((call) => call.url === "/api/coordinator/restart")).toHaveLength(1),
    );
    expect(await within(block).findByText("Restarted")).toBeDefined();
  });

  it("stays away when no coordinator is running", async () => {
    const kit = await dialog({ live_agents: [] });
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    await changeModel(block);
    await waitFor(() => expect(kit.calls.some((call) => call.method === "PUT")).toBe(true));
    expect(within(block).queryByText("Restart the coordinator now")).toBeNull();
  });

  it("says why when the service cannot", async () => {
    await dialog(live, refusal(409, "refused", "no host to restart on"));
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    await changeModel(block);
    fireEvent.click(await within(block).findByRole("button", { name: "Restart the coordinator now" }));
    expect(await within(block).findByText(/no host to restart on/)).toBeDefined();
  });
});
