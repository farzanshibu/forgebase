import { defineConfig } from "vitest/config";
import type { Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

const BACKEND = "http://127.0.0.1:8090";

/**
 * `/` and `/admin` (no slash) redirect to `/admin/` in the dev and demo servers, as the real server does
 * (crates/forgebase-api/src/admin.rs); Vite would otherwise answer with its "did you mean /admin/" page.
 */
const adminRedirect: Plugin = {
  name: "admin-redirect",
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const [path, query] = (req.url ?? "").split("?", 2);
      if (path === "/" || path === "/admin") {
        res.statusCode = 302;
        res.setHeader("Location", `/admin/${query ? `?${query}` : ""}`);
        res.end();
        return;
      }
      next();
    });
  },
};

export default defineConfig({
  base: "/admin/",
  plugins: [adminRedirect, react(), tailwindcss()],
  resolve: {
    // monaco-editor only exports its package root and a few path patterns; the editor imports exactly the pieces it uses.
    alias: { "monaco-esm": fileURLToPath(new URL("./node_modules/monaco-editor/esm/vs", import.meta.url)) },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rolldownOptions: {
      output: {
        // Long-lived vendor chunks cache across releases; app code changes without invalidating them.
        codeSplitting: {
          groups: [
            { name: "react", test: /node_modules[\\/](react|react-dom|scheduler)[\\/]/, priority: 30 },
            { name: "router", test: /node_modules[\\/](react-router|react-router-dom)[\\/]/, priority: 20 },
          ],
        },
      },
    },
  },
  server: {
    proxy: {
      "/health": BACKEND,
      "/api": BACKEND,
      "/metrics": BACKEND,
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: false,
    // e2e/ is Playwright (its own package and runner); vitest must not collect journey.spec.ts.
    exclude: ["**/node_modules/**", "**/dist/**", "e2e/**"],
  },
});
