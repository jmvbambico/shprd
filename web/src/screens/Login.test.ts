// What the person reads when the password does not get them in.

import { describe, expect, it } from "vitest";

import { ApiError } from "../api/client";
import { loginMessage } from "./Login";

describe("loginMessage", () => {
  it("says how long a rate limit wants", () => {
    expect(loginMessage(new ApiError(429, "rate_limited", "too many", 90))).toBe(
      "Too many attempts. Try again in 90 s.",
    );
  });

  it("repeats the service's own line for a wrong password", () => {
    expect(loginMessage(new ApiError(401, "unauthorized", "that password did not work"))).toBe(
      "that password did not work",
    );
  });

  it("blames the service, not the password, when it cannot be reached", () => {
    expect(loginMessage(new ApiError(0, "network", "connection refused"))).toBe(
      "Can't reach shprd. Is the service running?",
    );
  });

  it("has a line for anything else", () => {
    expect(loginMessage(new Error("boom"))).toBe("boom");
    expect(loginMessage("not an error")).toBe("Something went wrong.");
  });
});
