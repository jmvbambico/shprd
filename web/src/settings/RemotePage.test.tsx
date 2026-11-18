// The Remote page's states: loading, a service without the route, an error, the
// tunnel off and on, the masked link, the QR, the refusal a public tunnel gets
// without a password, the sessions and their revoke, and logging out.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RemotePage } from "./RemotePage";
import { json, refusal, scripted, SESSIONS, tunnelOff, tunnelOn } from "./testkit";

afterEach(cleanup);

function open(routes: Parameters<typeof scripted>[0]) {
  const kit = scripted(routes);
  const onSignedOut = vi.fn();
  render(<RemotePage client={kit.api} onSignedOut={onSignedOut} />);
  return { ...kit, onSignedOut };
}

describe("RemotePage: states", () => {
  it("says it is reading", () => {
    open({
      "GET /api/tunnels": new Promise<Response>(() => undefined) as unknown as Response,
      "GET /api/sessions": new Promise<Response>(() => undefined) as unknown as Response,
    });
    expect(screen.getByText("Reading the tunnel…")).toBeDefined();
    expect(screen.getByText("Reading the sessions…")).toBeDefined();
  });

  it("degrades to 'not available' on a 404, without an error", async () => {
    open({});
    expect(await screen.findByText("Tunnels are not available on this service.")).toBeDefined();
    expect(await screen.findByText(/signed-in devices is not available/)).toBeDefined();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Log out" })).toBeDefined();
  });

  it("shows a failure as it is", async () => {
    open({
      "GET /api/tunnels": refusal(502, "internal", "the tunnel record is unreadable"),
      "GET /api/sessions": json(200, { sessions: [] }),
    });
    expect((await screen.findByRole("alert")).textContent).toBe("the tunnel record is unreadable");
    expect(await screen.findByText("No device is signed in.")).toBeDefined();
  });
});

describe("RemotePage: the tunnel", () => {
  it("offers the providers and starts the one picked", async () => {
    const { calls } = open({
      "GET /api/tunnels": json(200, tunnelOff()),
      "GET /api/sessions": json(200, SESSIONS),
      "POST /api/tunnels/start": json(200, tunnelOn()),
    });
    expect(await screen.findByText("Off")).toBeDefined();
    const select = screen.getByLabelText("Provider") as HTMLSelectElement;
    expect(select.value).toBe("tailscale");
    expect(within(select).getByText("cloudflared (public internet)")).toBeDefined();
    fireEvent.change(select, { target: { value: "cloudflared" } });
    fireEvent.click(screen.getByRole("button", { name: "Start tunnel" }));
    expect(await screen.findByText("On")).toBeDefined();
    expect(calls.find((c) => c.url === "/api/tunnels/start")?.body).toEqual({ provider: "cloudflared" });
  });

  it("shows the server's refusal when a public tunnel has no password", async () => {
    const words = "a public tunnel needs a login password; run `shprd web password set`";
    open({
      "GET /api/tunnels": json(200, tunnelOff({ password_set: false })),
      "GET /api/sessions": json(200, { sessions: [] }),
      "POST /api/tunnels/start": refusal(409, "refused", words),
    });
    await screen.findByText("Off");
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "cloudflared" } });
    expect(screen.getByText(/needs a login password first/)).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Start tunnel" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(words);
    expect(screen.getByText("Off")).toBeDefined();
  });

  it("masks the link until Reveal, shows the QR, and stops", async () => {
    const { calls } = open({
      "GET /api/tunnels": json(200, tunnelOn()),
      "GET /api/sessions": json(200, { sessions: [] }),
      "POST /api/tunnels/stop": json(200, tunnelOff()),
    });
    const link = await screen.findByText(/••••••/);
    expect(link.textContent).not.toContain("quiet-river");
    expect(document.body.textContent).not.toContain("quiet-river-1234");
    // The QR encodes the whole link, so it follows the mask.
    expect(screen.queryByRole("img", { name: "QR code for the link" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Reveal" }));
    expect(screen.getByText("https://quiet-river-1234.trycloudflare.com")).toBeDefined();
    const qr = screen.getByRole("img", { name: "QR code for the link" }) as HTMLImageElement;
    expect(qr.src.startsWith("data:image/svg+xml")).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(document.body.textContent).not.toContain("quiet-river-1234");
    expect(screen.queryByRole("img", { name: "QR code for the link" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop tunnel" }));
    expect(await screen.findByText("Off")).toBeDefined();
    expect(calls.some((c) => c.url === "/api/tunnels/stop")).toBe(true);
    expect(screen.queryByRole("img", { name: "QR code for the link" })).toBeNull();
  });

  it("says why there is no tunnel when the last one ended badly", async () => {
    open({
      "GET /api/tunnels": json(200, tunnelOff({ why: "cloudflared exited: no network" })),
      "GET /api/sessions": json(200, { sessions: [] }),
    });
    expect(await screen.findByText("cloudflared exited: no network")).toBeDefined();
  });
});

describe("RemotePage: sessions and logout", () => {
  it("lists devices, marks this one, and revokes another", async () => {
    let revoked = false;
    const { calls } = open({
      "GET /api/tunnels": json(200, tunnelOff()),
      "GET /api/sessions": () =>
        json(200, revoked ? { ...SESSIONS, sessions: SESSIONS.sessions.slice(1) } : SESSIONS),
      "DELETE /api/sessions/aaaaaaaaaaaa": () => {
        revoked = true;
        return json(200, { ok: true, revoked: true });
      },
    });
    const phone = await screen.findByText("phone · Safari");
    expect(phone).toBeDefined();
    expect(screen.getByText("this device")).toBeDefined();
    expect(screen.getByText("remembered")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Revoke phone · Safari (aaaaaaaaaaaa)" }));
    await waitFor(() => expect(screen.queryByText("phone · Safari")).toBeNull());
    expect(calls.some((c) => c.method === "DELETE" && c.url === "/api/sessions/aaaaaaaaaaaa")).toBe(true);
    expect(screen.getByText("desktop · Firefox")).toBeDefined();
  });

  it("signs the app out at once when this device's own session is revoked", async () => {
    const { onSignedOut } = open({
      "GET /api/tunnels": json(200, tunnelOff()),
      "GET /api/sessions": json(200, SESSIONS),
      "DELETE /api/sessions/bbbbbbbbbbbb": json(200, { ok: true, revoked: true }),
    });
    fireEvent.click(await screen.findByRole("button", { name: /Revoke desktop · Firefox/ }));
    await waitFor(() => expect(onSignedOut).toHaveBeenCalled());
  });

  it("shows the refusal when the password has to be entered again", async () => {
    open({
      "GET /api/tunnels": json(200, tunnelOff()),
      "GET /api/sessions": json(200, SESSIONS),
      "DELETE /api/sessions/aaaaaaaaaaaa": refusal(401, "reauth", "enter your password again"),
    });
    fireEvent.click(await screen.findByRole("button", { name: /Revoke phone/ }));
    expect((await screen.findByRole("alert")).textContent).toContain("enter your password again");
    expect(screen.getByText("phone · Safari")).toBeDefined();
  });

  it("logs out and tells the app", async () => {
    const { onSignedOut, calls } = open({
      "GET /api/tunnels": json(200, tunnelOff()),
      "GET /api/sessions": json(200, SESSIONS),
      "POST /api/logout": json(200, { ok: true, revoked: true }),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Log out" }));
    await waitFor(() => expect(onSignedOut).toHaveBeenCalled());
    expect(calls.some((c) => c.url === "/api/logout")).toBe(true);
  });
});
