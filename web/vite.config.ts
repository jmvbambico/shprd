import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The web app is served by `shprd serve` from the origin root, so it is built
 * for `/`: an SPA route such as `/tasks` must still load `/assets/…`.
 *
 * `vite dev` proxies `/api` to a service started by hand, so a developer can
 * run `npm run dev` against `shprd serve --port 7787` without CORS.
 */
export default defineConfig({
  base: "/",
  plugins: [react()],
  build: {
    outDir: "dist",
    assetsDir: "assets",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:7787",
    },
  },
});
