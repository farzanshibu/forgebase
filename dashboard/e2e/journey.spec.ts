/**
 * The ten-minute first journey from docs/requirements/dashboard-ux.md, walked in a real browser
 * against a real ForgeBase server. Recipe and expected results: docs/ux/walkthrough.md.
 *
 * Steps that need work not yet merged (setup wizard FB-92, function editor FB-94, the app-shell
 * package from docs/ux/work-packages.md) are `test.fixme` with the finding id from docs/ux/audit.md;
 * remove the fixme when the work lands.
 */
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page, type TestInfo } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EMAIL = process.env.FB_ADMIN_EMAIL ?? "";
const PASSWORD = process.env.FB_ADMIN_PASSWORD ?? "";
const SHOTS = process.env.FB_SHOTS ?? path.join(os.tmpdir(), "forgebase-ux");

/** Unique names per run and theme, so the light and dark projects never collide. */
const RUN = Date.now().toString(36).slice(-5);
const names = (theme: string) => ({
  collection: `ux_notes_${theme}_${RUN}`,
  bucket: `ux-uploads-${theme}-${RUN}`,
  fn: `ux-hello-${theme}-${RUN}`,
  token: `ux-token-${theme}-${RUN}`,
});

/** Every studio's main page, for the accessibility pass. */
const STUDIOS = ["", "database", "redis", "storage", "vector", "graph", "auth", "access", "identity", "functions", "jobs", "queues", "billing", "system"];

async function shot(page: Page, info: TestInfo, step: string) {
  fs.mkdirSync(SHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SHOTS, `journey-${step}-${info.project.name}.png`), fullPage: true });
}

/** The smallest valid ForgeBase guest: imports forgebase.output(ptr, len) and answers a constant JSON body. */
function helloWasm(text = '{"hello":"from a wasm function"}'): Buffer {
  const enc = new TextEncoder();
  const leb = (n: number) => { const o: number[] = []; do { let b = n & 0x7f; n >>>= 7; if (n) b |= 0x80; o.push(b); } while (n); return o; };
  const sleb = (n: number) => { const o: number[] = []; for (;;) { const b = n & 0x7f; n >>= 7; if ((n === 0 && !(b & 0x40)) || (n === -1 && b & 0x40)) { o.push(b); return o; } o.push(b | 0x80); } };
  const str = (s: string) => { const b = enc.encode(s); return [b.length, ...b]; };
  const sec = (id: number, bytes: number[]) => [id, ...leb(bytes.length), ...bytes];
  const body = [...enc.encode(text)];
  const code = [0x00, 0x41, 16, 0x41, ...sleb(body.length), 0x10, 0x00, 0x1a, 0x41, 0x00, 0x0b];
  return Buffer.from([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...sec(1, [2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]),
    ...sec(2, [1, ...str("forgebase"), ...str("output"), 0x00, 0]),
    ...sec(3, [1, 1]),
    ...sec(5, [1, 0x00, 1]),
    ...sec(7, [2, ...str("memory"), 0x02, 0, ...str("handle"), 0x00, 1]),
    ...sec(10, [1, ...leb(code.length), ...code]),
    ...sec(11, [1, 0x00, 0x41, 16, 0x0b, ...leb(body.length), ...body]),
  ]);
}

/** Picks an option in a PanelUI Select (the dashboard's `Sel`) by its trigger's accessible name. */
async function choose(page: Page, trigger: string, option: string) {
  await page.getByRole("combobox", { name: trigger }).click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

async function signIn(page: Page) {
  await page.goto("/admin/");
  await page.getByLabel("Email").fill(EMAIL);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("navigation", { name: "Studios" })).toBeVisible();
}

test.beforeAll(() => {
  if (!EMAIL || !PASSWORD) throw new Error("Set FB_ADMIN_EMAIL and FB_ADMIN_PASSWORD (a throwaway test admin). See docs/ux/walkthrough.md.");
});

test.beforeEach(async ({ page }, info) => {
  await page.addInitScript((mode) => {
    try { localStorage.setItem("forgebase.admin.theme", mode); } catch { /* storage blocked: the OS scheme still applies */ }
  }, info.project.name);
});

test.describe.serial("first journey", () => {
  let page: Page;
  let token = "";

  test.beforeAll(async ({ browser }, info) => {
    page = await browser.newPage({ colorScheme: info.project.name as "light" | "dark" });
    await page.addInitScript((mode) => { try { localStorage.setItem("forgebase.admin.theme", mode); } catch { /* ignore */ } }, info.project.name);
  });
  test.afterAll(async () => { await page?.close(); });

  // Step 1 (A-01): on a server with no administrator the dashboard opens the setup page, not a sign-in form.
  // Needs a fresh server (docs/ux/walkthrough.md, "Fresh server"): FB_SETUP_URL is the link `forgebase serve` prints.
  test("1. first-run setup creates the administrator in the browser", async () => {
    test.skip(!process.env.FB_SETUP_URL, "Needs a server with no administrator: set FB_SETUP_URL to the First run link that serve prints.");
    await page.goto(process.env.FB_SETUP_URL!);
    await expect(page.getByRole("heading", { name: /create the administrator/i })).toBeVisible();
    await page.getByLabel("Email").fill(EMAIL);
    await page.getByLabel("Password", { exact: true }).fill(PASSWORD);
    await page.getByLabel("Confirm password").fill(PASSWORD);
    await page.getByRole("button", { name: "Create administrator" }).click();
    await expect(page.getByRole("region", { name: "Feature map" })).toBeVisible();
    await shot(page, test.info(), "00-setup");
  });

  test("1b. sign in and land on Overview", async () => {
    const info = test.info();
    await signIn(page);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await shot(page, info, "01-overview");
  });

  // A-02: Overview has a Getting started checklist and a Connect your app panel.
  test("Overview shows Getting started and Connect your app", async () => {
    await page.goto("/admin/");
    await expect(page.getByRole("region", { name: "Getting started" })).toBeVisible();
    await expect(page.getByRole("region", { name: "Connect your app" })).toBeVisible();
  });

  test("2. create a collection with fields and add a record", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto("/admin/database");
    await page.getByRole("button", { name: "New collection" }).click();
    await page.getByLabel(/^Name/).fill(n.collection);
    await page.getByRole("button", { name: "Add field" }).click();
    await page.getByLabel("Field 1 name").fill("title");
    await page.getByLabel("Field 1 required").check();
    await page.getByRole("button", { name: "Add field" }).click();
    await page.getByLabel("Field 2 name").fill("done");
    await choose(page, "Field 2 type", "bool");
    await shot(page, info, "02a-schema");
    await page.getByRole("button", { name: "Create collection" }).click();
    await expect(page).toHaveURL(new RegExp(`/database/c/${n.collection}$`));

    // A-07: a typed form per field (a text box for title, a switch for done) opens in a side sheet.
    await page.getByRole("button", { name: "Add record" }).first().click();
    const form = page.getByRole("form", { name: "New record" });
    await form.getByLabel("title").fill("Hello from the walkthrough");
    await form.getByRole("button", { name: "Create record" }).click();
    await expect(page.getByText("Hello from the walkthrough")).toBeVisible();
    await shot(page, info, "02b-records");
  });

  // A-03: every resource shows a Use from code panel (JavaScript SDK, Rust SDK, curl).
  test("3. copy a working snippet from the collection's Use from code panel", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto(`/admin/database/c/${n.collection}`);
    await page.getByRole("button", { name: "Use from code" }).click();
    const panel = page.getByRole("region", { name: "Use from code" });
    await expect(panel).toContainText(n.collection);
    await expect(panel).toContainText("<YOUR_API_KEY>");
  });

  // A-08: one dialog makes a key with read access to one collection, without knowing the scope grammar.
  test("3b. the Create API key dialog makes a key that reads the collection over HTTP", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto("/admin/auth/keys?new=1");
    const form = page.getByRole("form", { name: "Create API key" });
    await form.getByLabel("App name").fill(n.token);
    const records = form.getByRole("group", { name: "Records" });
    await records.getByRole("checkbox", { name: "Read records" }).check();
    await records.getByRole("checkbox", { name: "All collections" }).uncheck();
    await records.getByRole("checkbox", { name: n.collection }).check();
    await expect(form).toContainText(`collections:read:${n.collection}`);
    await form.getByRole("button", { name: "Create key" }).click();
    const secret = page.getByLabel("Secret");
    await expect(secret).toBeVisible();
    token = (await secret.innerText()).trim();
    await expect(page.getByLabel("curl example")).toContainText("<YOUR_API_KEY>");
    await shot(page, info, "03-token");
    await page.getByRole("button", { name: "Done" }).click();
    const res = await page.request.get(`/api/v1/collections/${n.collection}/records`, { headers: { "X-API-Key": token } });
    expect(res.status(), await res.text()).toBe(200);
    expect(JSON.stringify(await res.json())).toContain("Hello from the walkthrough");
  });

  test("4. upload a file to a bucket", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto("/admin/storage");
    await page.getByRole("button", { name: "New bucket" }).first().click();
    await page.getByLabel(/^Name/).fill(n.bucket);
    await page.getByRole("button", { name: "Create bucket" }).click();
    // creating a bucket opens it; drop a file into the upload area
    await expect(page.getByRole("heading", { name: n.bucket })).toBeVisible();
    await page.getByLabel("Choose files").setInputFiles({ name: "hello.txt", mimeType: "text/plain", buffer: Buffer.from("hello from the walkthrough\n") });
    await expect(page.getByRole("button", { name: "Select hello.txt" })).toBeVisible();
    await shot(page, info, "04-upload");
  });

  // A-05: write the function in the browser from a template that reads a collection, then test it (FB-101).
  // The template starts on the newest collection; a deploy opens the Test tab.
  test("5. write, deploy and test a function that reads the collection", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto("/admin/functions");
    await page.getByRole("button", { name: /new function/i }).first().click();
    await page.getByRole("button", { name: /read a collection/i }).click();
    await page.getByRole("button", { name: /deploy/i }).click();
    await page.getByRole("tab", { name: "Test" }).click();
    await page.getByRole("button", { name: /run test/i }).click();
    await expect(page.getByLabel("Response body")).toContainText(n.collection);
  });

  test("5b. deploy a WebAssembly function and call it", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto("/admin/functions");
    await page.getByRole("button", { name: "Upload .wasm" }).click();
    const form = page.getByRole("form", { name: "Deploy function" });
    await form.getByLabel("Name", { exact: true }).fill(n.fn);
    await form.getByLabel("WebAssembly module").setInputFiles({ name: "hello.wasm", mimeType: "application/wasm", buffer: helloWasm() });
    await form.getByRole("button", { name: "Deploy" }).click();
    // A deploy opens the function's Versions tab.
    await expect(page).toHaveURL(new RegExp(`/functions/f/${n.fn}$`));
    await expect(page.getByRole("button", { name: "Deploy new version" })).toBeVisible();
    await shot(page, info, "05-functions");
    const status = await page.evaluate(async (fn) => {
      const csrf = document.cookie.split(";").map((c) => c.trim()).find((c) => c.startsWith("fb_csrf="))?.slice(8) ?? "";
      const r = await fetch(`/api/v1/functions/${fn}/invoke`, { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json", "X-CSRF-Token": decodeURIComponent(csrf) }, body: "{}" });
      return r.status;
    }, n.fn);
    expect(status).toBe(200);
  });

  test("6. see the function's logs", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.goto(`/admin/functions/f/${n.fn}/logs`);
    await expect(page.getByRole("list", { name: "Executions" }).getByRole("listitem").first()).toBeVisible();
    await shot(page, info, "06-logs");
  });

  // A-04: an error shows plain words, the next step and a copyable request id, with raw JSON behind Details.
  // ErrorPanel (FB-98) does this; CollectionView still draws its own "No collection named …" line (FB-100).
  test("6b. an error explains its fix and offers the request id", async () => {
    await page.goto("/admin/database/c/does_not_exist");
    const err = page.getByRole("alert");
    await expect(err).toContainText(/what to do/i);
    await expect(err.getByRole("button", { name: /copy request id/i })).toBeVisible();
  });

  // A-06: Cmd/Ctrl+K opens a command palette that jumps to any studio, resource or action.
  test("command palette jumps to the new collection", async () => {
    const info = test.info();
    const n = names(info.project.name);
    await page.keyboard.press(process.platform === "darwin" ? "Meta+K" : "Control+K");
    await page.getByRole("combobox", { name: /search/i }).fill(n.collection);
    // Resources load when the palette opens; Enter picks the highlighted match once it is listed.
    await expect(page.getByRole("option", { name: new RegExp(n.collection) })).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(new RegExp(n.collection));
  });
});

test.describe("accessibility: no serious or critical axe issue on each studio's main page", () => {
  for (const studio of STUDIOS) {
    test(`/${studio}`, async ({ page }) => {
      await signIn(page);
      await page.goto(`/admin/${studio}`);
      await page.waitForLoadState("networkidle");
      const { violations } = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"]).analyze();
      const bad = violations.filter((v) => v.impact === "serious" || v.impact === "critical");
      expect(bad.map((v) => `${v.id}: ${v.help} (${v.nodes.length})`)).toEqual([]);
    });
  }
});
