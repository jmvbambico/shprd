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
