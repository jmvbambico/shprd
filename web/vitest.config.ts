import { defineConfig } from "vitest/config";

/**
 * The client and the layout are plain TypeScript, so the tests run in Node with
 * no DOM: `fetch` and `WebSocket` are injected ([`ApiClient`]), which is also
 * what keeps a test off this machine's real network.
 */
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
