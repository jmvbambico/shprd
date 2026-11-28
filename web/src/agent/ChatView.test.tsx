// The chat's states: loading, empty, data, error; and a turn sent.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { json, refusal, scripted, type Answer } from "../settings/testkit";
import { ChatView } from "./ChatView";

afterEach(cleanup);

/** A change feed that never answers, so the screen's own reads are all there is. */
const QUIET: Answer = () => new Promise<Response>(() => undefined);

function chatPage(over: Record<string, unknown> = {}) {
  return {
    space: "coordinator",
    program: "claude",
    session: "s1",
    messages: [],
    cursor: "c1",
    more: false,
    note: null,
    ...over,
  };
}

describe("ChatView", () => {
  it("says it is reading, before the first page", () => {
    const { api } = scripted({ "GET /api/chat": () => new Promise<Response>(() => undefined), "GET /api/changes": QUIET });
    render(<ChatView client={api} space="coordinator" />);
    expect(screen.getByRole("status").textContent).toContain("Reading the conversation");
  });

  it("shows the coordinator's empty words and the service's note", async () => {
    const { api } = scripted({
      "GET /api/chat": json(200, chatPage({ note: "claude has not written a session for /work yet" })),
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="coordinator" />);
    await screen.findByText("No messages yet");
    expect(screen.getByText("The coordinator plans the work and keeps you posted.")).toBeDefined();
    expect(screen.getByText("claude has not written a session for /work yet")).toBeDefined();
  });

  it("uses the task words for a task's empty chat", async () => {
    const { api } = scripted({
      "GET /api/chat": json(200, chatPage({ space: "T1" })),
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="T1" />);
    await screen.findByText("The agent's conversation shows here as it writes it.");
  });

  it("draws both sides, markdown safely, tool calls and the start marker", async () => {
    const { api } = scripted({
      "GET /api/chat": json(
        200,
        chatPage({
          messages: [
            { role: "user", text: "run the tests", tools: [], at_ms: Date.UTC(2026, 10, 18, 9, 0) },
            {
              role: "assistant",
              text: "**All green.** <img src=x onerror=alert(1)> [docs](javascript:alert(1))",
              tools: [{ name: "Bash", summary: "cargo test" }],
              at_ms: null,
            },
          ],
        }),
      ),
      "GET /api/changes": QUIET,
    });
    const { container } = render(<ChatView client={api} space="coordinator" />);
    await screen.findByText("Start of conversation");
    const rows = container.querySelectorAll(".chat-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]?.getAttribute("data-from")).toBe("you");
    expect(rows[1]?.getAttribute("data-from")).toBe("agent");
    expect(container.querySelector("strong")?.textContent).toBe("All green.");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("a[href]")).toBeNull();
    expect(screen.getByText("→ Bash · cargo test")).toBeDefined();
  });

  it("folds a long reply under Show more", async () => {
    const long = Array.from({ length: 40 }, (_, i) => `line ${i}`).join("\n\n");
    const { api } = scripted({
      "GET /api/chat": json(200, chatPage({ messages: [{ role: "assistant", text: long, tools: [], at_ms: 5 }] })),
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="coordinator" />);
    const more = await screen.findByRole("button", { name: "Show more" });
    fireEvent.click(more);
    expect(screen.getByRole("button", { name: "Show less" })).toBeDefined();
  });

  it("shows a failed read with a way to try again, keeping nothing false", async () => {
    let calls = 0;
    const { api } = scripted({
      "GET /api/chat": () => {
        calls += 1;
        return calls === 1 ? refusal(500, "internal", "the transcript could not be read") : json(200, chatPage());
      },
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="coordinator" />);
    await screen.findByText(/the transcript could not be read/);
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("No messages yet");
  });

  it("sends the composer's words with Enter and clears it", async () => {
    const { api, calls } = scripted({
      "GET /api/chat": json(200, chatPage()),
      "POST /api/chat": json(200, { space: "coordinator", sent: true }),
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="coordinator" />);
    const box = (await screen.findByPlaceholderText("Message the coordinator")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "  status?  " } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(box.value).toBe(""));
    const post = calls.find((call) => call.method === "POST");
    expect(post?.body).toEqual({ space: "coordinator", text: "status?" });
  });

  it("keeps the words and says why when a send is refused", async () => {
    const { api } = scripted({
      "GET /api/chat": json(200, chatPage()),
      "POST /api/chat": refusal(409, "refused", "no open tab for task T1. Nothing was sent"),
      "GET /api/changes": QUIET,
    });
    render(<ChatView client={api} space="T1" />);
    const box = (await screen.findByPlaceholderText("Message the agent")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await screen.findByText(/Nothing was sent/);
    expect(box.value).toBe("hello");
  });

  it("does not send an empty message", async () => {
    const { api, calls } = scripted({ "GET /api/chat": json(200, chatPage()), "GET /api/changes": QUIET });
    render(<ChatView client={api} space="coordinator" />);
    await screen.findByText("No messages yet");
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    expect(calls.some((call) => call.method === "POST")).toBe(false);
  });

  it("starts over when the space changes", async () => {
    const { api, calls } = scripted({
      "GET /api/chat": (call) =>
        json(200, chatPage({ space: call.url.includes("T2") ? "T2" : "coordinator", messages: call.url.includes("T2") ? [{ role: "assistant", text: "in T2", tools: [], at_ms: 1 }] : [] })),
      "GET /api/changes": QUIET,
    });
    const { rerender } = render(<ChatView client={api} space="coordinator" />);
    await screen.findByText("No messages yet");
    rerender(<ChatView client={api} space="T2" />);
    await screen.findByText("in T2");
    // The new space's first read asks from the start, not from the old cursor.
    const asked = calls.filter((call) => call.url.includes("T2"));
    expect(asked[0]?.url).not.toContain("since=");
  });
});
