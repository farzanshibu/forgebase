import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

const BACKEND = "http://127.0.0.1:8090";

export default defineConfig({
  base: "/admin/",
  plugins: [react(), tailwindcss()],
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
  },
});
