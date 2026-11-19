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
    expect(reviewer.title).toBe("Reviewer");
    expect(reviewer.name.value).toBe("Reviewer");
  });
});

describe("the Name row", () => {
  it("writes the trimmed name when the person leaves the box, and an empty box removes it", async () => {
    const kit = scripted(
      reads({ "PUT /api/settings": json(200, { outcome: "stored", settings: settings() }) }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    const worker = await screen.findByRole("region", { name: "Worker" });
    const box = within(worker).getByLabelText("Worker name");
    fireEvent.change(box, { target: { value: "  Dev " } });
    fireEvent.blur(box);
    await waitFor(() =>
      expect(kit.calls.find((call) => call.method === "PUT")?.body).toEqual({
        key: "roles.worker.title",
        value: "Dev",
      }),
    );
  });

  it("an empty name goes back to the built-in one", async () => {
    const kit = scripted(
      reads({
        "GET /api/settings": json(200, named),
        "PUT /api/settings": json(200, { outcome: "removed", settings: named }),
      }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    const worker = await screen.findByRole("region", { name: "Dev" });
    const box = within(worker).getByLabelText("Dev name");
    fireEvent.change(box, { target: { value: "  " } });
    fireEvent.blur(box);
    await waitFor(() =>
      expect(kit.calls.find((call) => call.method === "PUT")?.body).toEqual({ key: "roles.worker.title" }),
    );
  });
});

describe("Restart the coordinator", () => {
  it("is on the coordinator's block and asks the service once", async () => {
    const kit = scripted(
      reads({
        "GET /api/settings": json(200, named),
        "POST /api/coordinator/restart": new Response(null, { status: 204 }),
      }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    expect(within(screen.getByRole("region", { name: "Dev" })).queryByText("Restart the coordinator now")).toBeNull();
    fireEvent.click(within(block).getByRole("button", { name: "Restart the coordinator now" }));
    await waitFor(() =>
      expect(kit.calls.filter((call) => call.url === "/api/coordinator/restart")).toHaveLength(1),
    );
    expect(await within(block).findByText("Restarted")).toBeDefined();
  });

  it("says why when the service cannot", async () => {
    const kit = scripted(
      reads({
        "GET /api/settings": json(200, named),
        "POST /api/coordinator/restart": refusal(409, "refused", "no host to restart on"),
      }),
    );
    render(<SettingsDialog client={kit.api} onClose={vi.fn()} onSignedOut={vi.fn()} />);
    const block = await screen.findByRole("region", { name: "Orchestrator" });
    fireEvent.click(within(block).getByRole("button", { name: "Restart the coordinator now" }));
    expect((await within(block).findByRole("alert")).textContent).toMatch(/no host to restart on/);
  });
});
