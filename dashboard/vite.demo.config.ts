// Demo mode: `npm run demo` serves the dashboard with every API answered by in-browser fakes (see demo/main.tsx).
// Same app and styles as `npm run dev`; only the entry changes and nothing is proxied to a server.
import { defineConfig, mergeConfig, type Plugin } from "vite";
import { fileURLToPath } from "node:url";
import base from "./vite.config.ts";

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const demoEntry: Plugin = {
  name: "demo-entry",
  transformIndexHtml: (html) => html.replace("/src/main.tsx", "/demo/main.tsx"),
};

export default mergeConfig(
  base,
  defineConfig({
    plugins: [demoEntry],
    resolve: {
      alias: {
        vitest: here("./demo/vitest-shim.ts"),
        "@testing-library/react": here("./demo/tl-shim.ts"),
        "@testing-library/user-event": here("./demo/tl-shim.ts"),
      },
    },
    server: { host: "127.0.0.1", port: 5173, strictPort: true, proxy: {} },
  }),
);
