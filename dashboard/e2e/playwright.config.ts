import { defineConfig } from "@playwright/test";
import os from "node:os";
import path from "node:path";

/**
 * Browser walkthrough of the dashboard's first journey (docs/requirements/dashboard-ux.md, "The test").
 * Runs against a real ForgeBase server; see docs/ux/walkthrough.md for the recipe.
 *
 * FB_URL              server base URL (default http://127.0.0.1:18110)
 * FB_ADMIN_EMAIL      administrator email (required)
 * FB_ADMIN_PASSWORD   administrator password (required; a throwaway test password)
 * FB_SHOTS            screenshot folder (default: <tmp>/forgebase-ux); keep it outside the repo
 *
 * Uses the installed Google Chrome (channel "chrome"); it never downloads browsers.
 */
export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts$/,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  outputDir: path.join(process.env.FB_SHOTS ?? path.join(os.tmpdir(), "forgebase-ux"), "results"),
  use: {
    baseURL: process.env.FB_URL ?? "http://127.0.0.1:18110",
    channel: "chrome",
    headless: true,
    viewport: { width: 1280, height: 860 },
    trace: "retain-on-failure",
  },
  projects: [
    { name: "light", use: { colorScheme: "light" } },
    { name: "dark", use: { colorScheme: "dark" } },
  ],
});
