import { defineConfig } from "vitest/config";

/**
 * The client, the layout and the graph's arithmetic are plain TypeScript, so
 * their tests run in Node with no DOM: `fetch` and `WebSocket` are injected
 * ([`ApiClient`]), which is also what keeps a test off this machine's real
 * network.
 *
 * The screens are drawn with React, so their files declare
 * `// @vitest-environment jsdom` and run in a DOM of their own instead.
 */
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
