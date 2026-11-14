import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { FunctionInfo } from "../../api";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installQueues } from "../../test/queueMock";
import { pick } from "../../test/pick";
import { sheet, toast } from "../../test/dataStudioMock";
import { checkWebhookUrl, clearSecret, parseSeconds, parseTtl, peekSecret, signedCurl, stashSecret, verifySnippet } from "./webhook";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const fnInfo = (name: string, description = ""): FunctionInfo => ({
  id: `f_${name}`, name, description, access: "permission", active_version: 1, latest_version: 1, principal: `service:function.${name}`,
  created_at: "2026-11-09T10:00:00Z", updated_at: "2026-11-09T10:00:00Z", created_by: "admin",
});

function setup(functions: FunctionInfo[] | "forbidden" = [fnInfo("notify", "Tells the team"), fnInfo("audit-log")]) {
  const mock = signedIn(new MockApi().install());
  const st = installQueues(mock);
  mock.on("GET", "/api/v1/functions", () => (functions === "forbidden" ? { status: 403, body: { error: { code: "forbidden", message: "no" } } } : { status: 200, body: { items: functions } }));
  return { mock, st };
}

const POST = "post /api/v1/queues/subscriptions";

async function openSheet(path = "/queues/t/orders") {
  renderStudio(path);
  await userEvent.click(await screen.findByRole("button", { name: "New subscription" }));
  return sheet("New subscription");
}
const type = (form: HTMLElement, label: RegExp, text: string) => userEvent.type(within(form).getByLabelText(label), text);
const submit = (form: HTMLElement) => userEvent.click(within(form).getByRole("button", { name: "Create subscription" }));

describe("checkWebhookUrl", () => {
  it("accepts public https URLs", () => {
    for (const u of ["https://example.com/hook", "https://hooks.example.com:8443/a?b=1", "https://93.184.216.34/x", "https://[2606:4700::1111]/x"]) expect(checkWebhookUrl(u)).toEqual({});
  });

  it("refuses what the server refuses: no URL, other schemes, credentials, no host, over 2048 bytes", () => {
    expect(checkWebhookUrl("  ").error).toMatch(/Enter the URL/);
    expect(checkWebhookUrl("example.com/hook").error).toMatch(/does not look like a URL/);
    expect(checkWebhookUrl("ftp://example.com/x").error).toMatch(/must start with https/);
    expect(checkWebhookUrl("https://user:pw@example.com/x").error).toMatch(/credentials/);
    expect(checkWebhookUrl("https://user@example.com/x").error).toMatch(/credentials/);
    expect(checkWebhookUrl(`https://example.com/${"a".repeat(2100)}`).error).toMatch(/2048/);
  });

  it("refuses localhost and literal loopback, private, link-local and reserved addresses", () => {
    for (const u of ["https://localhost/x", "https://api.localhost/x", "https://127.0.0.1/x", "https://10.1.2.3/x", "https://172.16.0.1/x", "https://172.31.255.255/x",
      "https://192.168.1.1/x", "https://169.254.169.254/latest", "https://0.0.0.0/x", "https://100.64.0.1/x", "https://224.0.0.1/x", "https://2130706433/x",
      "https://[::1]/x", "https://[fe80::1]/x", "https://[fd00::1]/x", "https://[::ffff:10.0.0.1]/x"]) {
      expect(checkWebhookUrl(u).error, u).toBeTruthy();
    }
    expect(checkWebhookUrl("https://172.32.0.1/x")).toEqual({});
    expect(checkWebhookUrl("https://11.0.0.1/x")).toEqual({});
  });

  it("only warns about http: the server decides with [queue] allow_http_push", () => {
    const c = checkWebhookUrl("http://example.com/x");
    expect(c.error).toBeUndefined();
    expect(c.warning).toMatch(/allow_http_push/);
    expect(checkWebhookUrl("http://127.0.0.1/x").error).toMatch(/loopback/);
  });
});

describe("push option helpers", () => {
  it("parses lifetimes with units and keeps the server's bounds (60 s to 31 days)", () => {
    expect(parseTtl("")).toEqual({ ok: true });
    expect(parseTtl("90s")).toEqual({ ok: true, ms: 90_000 });
    expect(parseTtl("30m")).toEqual({ ok: true, ms: 1_800_000 });
    expect(parseTtl("1h 30m")).toEqual({ ok: true, ms: 5_400_000 });
    expect(parseTtl("7D")).toEqual({ ok: true, ms: 604_800_000 });
    expect(parseTtl("31d")).toEqual({ ok: true, ms: 31 * 86_400_000 });
    for (const bad of ["soon", "30", "10x", "59s", "32d"]) expect(parseTtl(bad).ok, bad).toBe(false);
    expect(parseSeconds("1.5", "Backoff base")).toEqual({ ok: true, ms: 1500 });
    expect(parseSeconds("", "x")).toEqual({ ok: true });
    expect(parseSeconds("-1", "Backoff base").ok).toBe(false);
  });

  it("holds a created secret in memory until it is cleared", () => {
    stashSecret("a", "whsec_x");
    expect(peekSecret("a")).toBe("whsec_x");
    clearSecret("a");
    expect(peekSecret("a")).toBeUndefined();
  });

  it("builds the verification snippets", () => {
    expect(verifySnippet()).toContain('from "@forgebase/sdk/webhooks"');
    expect(verifySnippet()).toContain("constructWebhookEvent");
    expect(signedCurl("https://example.com/h")).toContain("X-Forgebase-Signature");
  });
});

describe("New subscription: delivery modes", () => {
  it("offers pull, push to a webhook and push to a function, and shows only the fields of the chosen one", async () => {
    setup();
    const form = await openSheet();
    expect(within(form).queryByLabelText(/^Endpoint URL/)).not.toBeInTheDocument();
    expect(within(form).queryByLabelText(/^Pause after failures/)).not.toBeInTheDocument();

    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    expect(within(form).getByLabelText(/^Endpoint URL/)).toBeInTheDocument();
    expect(within(form).getByLabelText(/^Pause after failures/)).toBeInTheDocument();
    expect(within(form).queryByRole("combobox", { name: "Function" })).not.toBeInTheDocument();

    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    expect(within(form).queryByLabelText(/^Endpoint URL/)).not.toBeInTheDocument();
    expect(within(form).getByRole("combobox", { name: "Function" })).toBeInTheDocument();
    expect(within(form).getByLabelText(/^Pause after failures/)).toBeInTheDocument();

    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "pull");
    expect(within(form).queryByRole("combobox", { name: "Function" })).not.toBeInTheDocument();
    expect(within(form).queryByLabelText(/^Pause after failures/)).not.toBeInTheDocument();
    // The expiry, filter, backoff and the other options apply to every mode.
    for (const l of [/^Expires after/, /^Filter/, /^Backoff base/, /^Backoff cap/, /^Max attempts/, /^Dead-letter topic/, /^Ack deadline/]) expect(within(form).getByLabelText(l)).toBeInTheDocument();
  });

  it("does not load the functions until a function push is chosen", async () => {
    const { mock } = setup();
    const form = await openSheet();
    const listed = () => mock.calls.filter((c) => c === "get /api/v1/functions").length;
    const before = listed();
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    expect(listed()).toBe(before);
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    await waitFor(() => expect(listed()).toBe(before + 1));
  });

  it("keeps creating a pull subscription exactly as before (no push fields in the body)", async () => {
    const { mock } = setup();
    const form = await openSheet();
    await type(form, /^Name/, "billing");
    await userEvent.clear(within(form).getByLabelText(/^Ack deadline/));
    await type(form, /^Ack deadline/, "45");
    await type(form, /^Max attempts/, "5");
    await userEvent.click(within(form).getByRole("checkbox", { name: /^Ordered/ }));
    await submit(form);
    await toast("Subscription billing created");
    expect(mock.bodies[POST]).toEqual([{ name: "billing", topic: "orders", ack_deadline_ms: 45_000, ordering: true, start: "latest", max_delivery_attempts: 5 }]);
    expect(await screen.findByLabelText("Subscription settings")).toHaveTextContent("Deliverypull");
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
  });
});

describe("New subscription: webhook", () => {
  it("checks the URL as you type and blocks the form until it is good", async () => {
    const { mock } = setup();
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    const create = within(form).getByRole("button", { name: "Create subscription" });
    expect(create).toBeDisabled();

    const url = within(form).getByLabelText(/^Endpoint URL/);
    await userEvent.type(url, "https://10.0.0.5/hook");
    expect(within(form).getByText(/10\.0\.0\.5 is in the private range/)).toBeInTheDocument();
    expect(create).toBeDisabled();

    await userEvent.clear(url);
    await userEvent.type(url, "https://user:pw@example.com/hook");
    expect(within(form).getByText(/credentials are not allowed/)).toBeInTheDocument();
    await userEvent.clear(url);
    await userEvent.type(url, "https://localhost:3000/hook");
    expect(within(form).getByText(/localhost cannot be reached/)).toBeInTheDocument();

    await userEvent.clear(url);
    await userEvent.type(url, "http://example.com/hook");
    expect(within(form).getByRole("note")).toHaveTextContent(/allow_http_push/);
    expect(create).toBeEnabled();

    await userEvent.clear(url);
    await userEvent.type(url, "https://example.com/hook");
    expect(within(form).queryByRole("note")).not.toBeInTheDocument();
    expect(create).toBeEnabled();
    expect(mock.bodies[POST]).toBeUndefined();
  });

  it("sends the push options and shows the signing secret once, on the new subscription's page", async () => {
    const { mock, st } = setup();
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "https://example.com/hook");
    await type(form, /^Pause after failures/, "5");
    await type(form, /^Expires after/, "2h");
    await type(form, /^Backoff base/, "2");
    await type(form, /^Backoff cap/, "120");
    await type(form, /^Max attempts/, "8");
    await type(form, /^Dead-letter topic/, "orders-dead");
    await userEvent.click(within(form).getByLabelText(/^Filter/));
    await userEvent.paste('{"kind":{"eq":"rush"}}');
    await submit(form);
    await toast("Subscription hooked created");

    expect(mock.bodies[POST]).toEqual([{
      name: "hooked", topic: "orders", ack_deadline_ms: 30_000, ordering: false, start: "latest", max_delivery_attempts: 8, dead_letter_topic: "orders-dead",
      backoff_base_ms: 2000, backoff_max_ms: 120_000, filter: { kind: { eq: "rush" } }, ttl_ms: 7_200_000,
      push: { kind: "webhook", endpoint: "https://example.com/hook" }, pause_after_failures: 5,
    }]);
    // The stored subscription never carries the secret: the create answer was its only appearance.
    expect(st.subs.at(-1)).not.toHaveProperty("push_secret");

    const banner = await screen.findByRole("status", { name: "Signing secret for hooked" });
    expect(within(banner).getByTestId("secret-once")).toHaveTextContent(/^whsec_created_/);
    expect(within(banner).getByRole("button", { name: "Copy secret" })).toBeInTheDocument();
    expect(banner).toHaveTextContent("Store it now: it won't be shown again");
    expect(within(banner).getByLabelText("Verify in Node")).toHaveTextContent('@forgebase/sdk/webhooks');
    // These two use the subscription, which loads after the notice appears.
    await waitFor(() => expect(within(banner).getByLabelText("Try your receiver with curl")).toHaveTextContent("https://example.com/hook"));
    expect(await within(banner).findByLabelText("Test against localhost")).toHaveTextContent("forgebase queues listen 'orders'");
    expect(await screen.findByLabelText("Subscription settings")).toHaveTextContent("push to webhook https://example.com/hook");
  });

  it("never shows the secret again once the page is left or the notice is dismissed", async () => {
    setup();
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "https://example.com/hook");
    await submit(form);
    const banner = await screen.findByRole("status", { name: "Signing secret for hooked" });
    expect(within(banner).getByTestId("secret-once")).toBeInTheDocument();

    // Away to the topic and back, without dismissing: the notice is gone.
    await userEvent.click(await screen.findByRole("link", { name: "orders" }));
    await screen.findByRole("table", { name: "Messages" });
    await userEvent.click(await screen.findByRole("link", { name: "hooked" }));
    await screen.findByLabelText("Subscription settings");
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
    expect(screen.queryByRole("status", { name: /Signing secret/ })).not.toBeInTheDocument();
  });

  it("dismisses the notice with I have copied it", async () => {
    setup();
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "https://example.com/hook");
    await submit(form);
    const banner = await screen.findByRole("status", { name: "Signing secret for hooked" });
    await userEvent.click(within(banner).getByRole("button", { name: "I have copied it" }));
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
  });

  it("does not show the secret of a subscription that was only opened", async () => {
    const { st } = setup();
    st.subs.push({ ...st.subs[0]!, name: "old-hook", push: { kind: "webhook", endpoint: "https://example.com/old" } });
    renderStudio("/queues/s/old-hook");
    await screen.findByLabelText("Subscription settings");
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
  });
});

describe("New subscription: function", () => {
  it("lists the deployed functions, sends the chosen one as the endpoint and shows no secret", async () => {
    const { mock } = setup();
    const form = await openSheet();
    await type(form, /^Name/, "fn-sub");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    const picker = within(form).getByRole("combobox", { name: "Function" });
    expect(within(form).getByRole("button", { name: "Create subscription" })).toBeDisabled();
    await userEvent.click(picker);
    const options = within(await screen.findByRole("listbox")).getAllByRole("option").map((o) => o.textContent?.trim());
    expect(options).toEqual(["notify: Tells the team", "audit-log"]);
    await userEvent.keyboard("{Escape}");
    await pick(picker, "notify");
    await type(form, /^Pause after failures/, "3");
    await submit(form);
    await toast("Subscription fn-sub created");
    expect(mock.bodies[POST]).toEqual([{ name: "fn-sub", topic: "orders", ack_deadline_ms: 30_000, ordering: false, start: "latest", push: { kind: "function", endpoint: "notify" }, pause_after_failures: 3 }]);
    expect(await screen.findByLabelText("Subscription settings")).toHaveTextContent("push to function notify");
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
    expect(screen.queryByRole("status", { name: /Signing secret/ })).not.toBeInTheDocument();
  });

  it("says so when no function is deployed or the list cannot load", async () => {
    setup([]);
    const form = await openSheet();
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    expect(await within(form).findByText("No functions deployed yet")).toBeInTheDocument();
  });

  it("explains a failed function list", async () => {
    setup("forbidden");
    const form = await openSheet();
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    expect(await within(form).findByText("The function list could not be loaded.")).toBeInTheDocument();
  });
});

describe("New subscription: options and errors", () => {
  it("checks the push options as you type", async () => {
    setup();
    const form = await openSheet();
    await type(form, /^Name/, "x");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "https://example.com/hook");
    const create = within(form).getByRole("button", { name: "Create subscription" });
    await type(form, /^Pause after failures/, "5000");
    expect(within(form).getByText(/whole number from 1 to 1000/)).toBeInTheDocument();
    expect(create).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Pause after failures/));
    await type(form, /^Expires after/, "5s");
    expect(within(form).getByText("At least 1 minute (60s).")).toBeInTheDocument();
    expect(create).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Expires after/));
    await type(form, /^Expires after/, "tomorrow");
    expect(within(form).getByText(/Use a number and a unit/)).toBeInTheDocument();
    await userEvent.clear(within(form).getByLabelText(/^Expires after/));
    await userEvent.click(within(form).getByLabelText(/^Filter/));
    await userEvent.paste("[1]");
    expect(within(form).getByText(/must be a JSON object/)).toBeInTheDocument();
    expect(create).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Filter/));
    expect(create).toBeEnabled();
  });

  it("says in plain words when the server refuses the endpoint, under the field, and keeps the sheet open", async () => {
    const { st, mock } = setup();
    st.refusePush = { code: "invalid_push_endpoint", message: "webhook URLs must be https://" };
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "http://example.com/hook");
    await submit(form);
    const field = await within(form).findByText(/ForgeBase cannot use that push endpoint\. webhook URLs must be https:\/\//);
    expect(field).toBeInTheDocument();
    expect(within(form).getByLabelText(/^Endpoint URL/)).toHaveAttribute("aria-invalid", "true");
    expect(form).toBeInTheDocument();
    expect(mock.bodies[POST]).toHaveLength(1);
    // Editing the URL clears the server's complaint.
    await userEvent.type(within(form).getByLabelText(/^Endpoint URL/), "s");
    expect(within(form).queryByText(/push endpoint\. webhook URLs/)).not.toBeInTheDocument();
  });

  it("maps a blocked address", async () => {
    const { st } = setup();
    st.refusePush = { code: "push_endpoint_blocked", message: "203.0.113.7 is in a reserved range; push endpoints must be public addresses" };
    const form = await openSheet();
    await type(form, /^Name/, "hooked");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Endpoint URL/, "https://203.0.113.7/hook");
    await submit(form);
    expect(await within(form).findByText(/private or reserved, so ForgeBase will not call it\. 203\.0\.113\.7 is in a reserved range/)).toBeInTheDocument();
  });

  it("explains a taken name next to the name", async () => {
    setup();
    const form = await openSheet();
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "webhook");
    await type(form, /^Name/, "order-workers");
    await type(form, /^Endpoint URL/, "https://example.com/hook");
    await submit(form);
    expect(await within(form).findByText(/A subscription with that name already exists\. Subscription names are unique/)).toBeInTheDocument();
    expect(within(form).getByLabelText(/^Name/)).toHaveAttribute("aria-invalid", "true");
  });

  it("shows other refusals in the sheet's alert with the friendly title", async () => {
    const { st } = setup();
    st.refusePush = { code: "invalid_name", message: "function name is not valid" };
    const form = await openSheet();
    await type(form, /^Name/, "fnsub");
    await pick(within(form).getByRole("combobox", { name: "Delivery" }), "function");
    await pick(within(form).getByRole("combobox", { name: "Function" }), "notify");
    await submit(form);
    expect(await within(form).findByRole("alert")).toHaveTextContent("That name is not allowed");
  });
});
