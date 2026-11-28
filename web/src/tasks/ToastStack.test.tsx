// The toast float: what it draws, what a tap does, and that a closed one stays closed.

// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToastStack } from "./ToastStack";
import type { Toast } from "./toasts";

afterEach(cleanup);

const toast = (id: string, extra: Partial<Toast> = {}): Toast => ({
  id,
  level: "success",
  title: `${id} · Landed`,
  body: "its work is on integration/m11",
  task: id,
  at: Date.now(),
  ...extra,
});

describe("ToastStack", () => {
  it("draws nothing when there is nothing to say", () => {
    render(<ToastStack toasts={[]} dismissed={new Set()} onDismiss={vi.fn()} onOpen={vi.fn()} />);
    expect(screen.queryByRole("list", { name: "Notifications" })).toBeNull();
  });

  it("shows at most three, newest last, and skips a dismissed one", () => {
    const list = ["a", "b", "c", "d"].map((id) => toast(id));
    render(<ToastStack toasts={list} dismissed={new Set(["c"])} onDismiss={vi.fn()} onOpen={vi.fn()} />);
    expect(screen.getAllByRole("listitem").map((li) => li.textContent?.slice(0, 1))).toEqual(["a", "b", "d"]);
  });

  it("a tap opens its task, and × dismisses it without opening", () => {
    const onOpen = vi.fn();
    const onDismiss = vi.fn();
    render(<ToastStack toasts={[toast("T1")]} dismissed={new Set()} onDismiss={onDismiss} onOpen={onOpen} />);
    fireEvent.click(screen.getByRole("button", { name: /T1 · Landed/ }));
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ id: "T1" }));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onDismiss).toHaveBeenCalledWith("T1");
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("lets one go when its time is up", () => {
    vi.useFakeTimers();
    try {
      render(
        <ToastStack toasts={[toast("T1", { at: Date.now() })]} dismissed={new Set()} onDismiss={vi.fn()} onOpen={vi.fn()} />,
      );
      expect(screen.getByRole("list", { name: "Notifications" })).toBeDefined();
      act(() => {
        vi.advanceTimersByTime(7000);
      });
      expect(screen.queryByRole("list", { name: "Notifications" })).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
