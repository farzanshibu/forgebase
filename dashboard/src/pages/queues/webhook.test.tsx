import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installQueues, pushAttempt, pushHealth, sub, topic } from "../../test/queueMock";
import { pick } from "../../test/pick";
import { confirmDialog, sheet, toast } from "../../test/dataStudioMock";
import { GRACE_OPTIONS, listenCommand, maskHeader, parsePauseAfter, publishCurl, redriveSummary, shq, subscribeCurl } from "./webhook";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const HOOK = { kind: "webhook", endpoint: "https://example.com/hook" } as const;

/** A queues mock with a push subscription `hook` (healthy, three attempts, one dead letter) next to the default pull ones. */
function setup(over: Parameters<typeof sub>[2] = {}) {
  const mock = signedIn(new MockApi().install());
  const st = installQueues(mock);
  st.subs.push(sub("hook", "orders", { push: HOOK, ...over }));
  st.health.hook = pushHealth();
  st.attempts.push(
    pushAttempt(1, { message_id: "msg_a" }),
    pushAttempt(2, { message_id: "msg_b", outcome: "failure", status: 503, error_class: "http_status", error: "endpoint answered 503", response_body: "unavailable", response_truncated: true, response_bytes: 9000,
      request_headers: { "content-type": "application/json", "x-forgebase-signature": "t=1731578400,v1=deadbeefcafe,v1=0123456789ab", authorization: "Bearer leaked" } }),
    pushAttempt(3, { message_id: "test-3", test: true }),
  );
  st.dead.hook = [
    { id: 11, subscription: "hook", topic: "orders", message_id: "msg_x", data: { n: 1 }, attributes: { kind: "rush" }, published_at: "2026-11-09T10:00:00Z", attempts: 3, history: [{ attempt: 3, at: "2026-11-09T10:00:00Z", outcome: "nack", error: "503" }], reason: "max delivery attempts reached", at: "2026-11-09T10:00:00Z" },
    { id: 12, subscription: "hook", topic: "orders", message_id: "msg_y", data: { n: 2 }, attributes: {}, published_at: "2026-11-09T10:00:00Z", attempts: 3, history: [], reason: "max delivery attempts reached", at: "2026-11-09T10:00:00Z", redriven_at: "2026-11-09T11:00:00Z", redriven_by: "admin" },
  ];
  return { mock, st };
}

describe("webhook helpers", () => {
  it("masks signatures and credentials even when the server did not", () => {
    expect(maskHeader("X-Forgebase-Signature", "t=1,v1=abc123,v1=def456")).toBe("t=1,v1=********,v1=********");
    expect(maskHeader("x-forgebase-signature", "t=1,v1=<redacted>")).toBe("t=1,v1=<redacted>");
    expect(maskHeader("Authorization", "Bearer x")).toBe("********");
    expect(maskHeader("content-type", "application/json")).toBe("application/json");
  });

  it("reads the auto-pause threshold, the grace choices and the copy-a-command strings", () => {
    expect(parsePauseAfter("")).toEqual({ ok: true, value: 0 });
    expect(parsePauseAfter(" 5 ")).toEqual({ ok: true, value: 5 });
    for (const bad of ["0", "1001", "x", "2.5", "-1"]) expect(parsePauseAfter(bad).ok).toBe(false);
    expect(GRACE_OPTIONS.map((g) => g.value)).toEqual(["0", "3600000", "86400000", "604800000"]);
    expect(shq("it's")).toBe(`'it'\\''s'`);
    expect(listenCommand("orders")).toBe("forgebase queues listen 'orders' --forward-to http://localhost:3000/webhooks");
    expect(listenCommand("audit", { "forgebase.op": "create" })).toContain(`--filter '{"forgebase.op":"create"}'`);
    expect(publishCurl("https://fb.example.com", "orders", { id: 1 })).toContain("'https://fb.example.com/api/v1/queues/topics/orders/publish'");
    expect(publishCurl("https://fb.example.com", "orders", { id: 1 })).toContain(`{"messages":[{"data":{"id":1}}]}`);
    expect(publishCurl("https://fb.example.com", "orders", {})).toContain("$FORGEBASE_TOKEN");
    expect(subscribeCurl("https://fb.example.com", "audit", { a: "b" })).toContain(`"filter":{"a":"b"}`);
    expect(redriveSummary({ redriven: [1, 2], already_redriven: [3], not_found: [], remaining: 4 })).toBe("Redriven 2 dead letters. 1 had been redriven before and were left alone. 4 more match: redrive again to deliver them.");
    expect(redriveSummary({ redriven: [1], already_redriven: [], not_found: [9], remaining: 0 })).toBe("Redriven 1 dead letter. 1 could not be found.");
  });
});

describe("Push subscription: endpoint health", () => {
  it("shows success rate, latency percentiles, consecutive failures and the last good and bad calls", async () => {
    const { st } = setup();
    st.health.hook = pushHealth({ consecutive_failures: 3, success_rate: 0.5, pause_after_failures: 5 });
    renderStudio("/queues/s/hook");
    const grid = await screen.findByLabelText("Endpoint health");
    expect(within(grid).getByText("50%")).toBeInTheDocument();
    expect(within(grid).getByText("110 ms")).toBeInTheDocument();
    expect(within(grid).getByText("480 ms")).toBeInTheDocument();
    expect(within(grid).getByText("900 ms")).toBeInTheDocument();
    expect(within(grid).getByText("Consecutive failures")).toBeInTheDocument();
    expect(within(grid).getByText("auto-pause at 5")).toBeInTheDocument();
    expect(await screen.findByText("connection refused")).toBeInTheDocument();
  });

  it("offers no webhook tools for a pull subscription", async () => {
    setup();
    renderStudio("/queues/s/order-workers");
    await screen.findByText("Settings");
    expect(screen.queryByLabelText("Endpoint health")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Send test event" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Rotate secret" })).not.toBeInTheDocument();
  });

  it("pauses with a reason, shows the paused reason and resumes", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Pause delivery" }));
    const dialog = await screen.findByRole("alertdialog", { name: /Pause delivery of hook/ });
    await userEvent.type(within(dialog).getByLabelText(/^Reason/), "deploying the receiver");
    await userEvent.click(within(dialog).getByRole("button", { name: "Pause delivery" }));
    await waitFor(() => expect(st.calls.pause).toEqual([{ reason: "deploying the receiver" }]));
    expect(await screen.findByTestId("paused-reason")).toHaveTextContent("deploying the receiver");
    expect(screen.getByRole("button", { name: "Resume delivery" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Resume delivery" }));
    await waitFor(() => expect(st.calls.resume).toBe(1));
    await toast("Delivery resumed");
    expect(await screen.findByRole("button", { name: "Pause delivery" })).toBeInTheDocument();
  });

  it("says when the circuit breaker paused it", async () => {
    const { st } = setup({ paused_at: "2026-11-09T10:00:00Z", paused_reason: "auto: 5 consecutive failed deliveries (last: connect)" });
    st.health.hook = pushHealth({ paused: true, consecutive_failures: 5 });
    renderStudio("/queues/s/hook");
    expect(await screen.findByText("Paused automatically")).toBeInTheDocument();
    expect(screen.getByTestId("paused-reason")).toHaveTextContent("auto: 5 consecutive failed deliveries");
    expect(screen.getByText(/none is dead-lettered while it is paused/)).toBeInTheDocument();
  });

  it("sets and clears pause_after_failures, and refuses a value out of range", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    const form = await screen.findByRole("form", { name: "Auto-pause" });
    const input = within(form).getByLabelText("Pause after failures");
    await userEvent.type(input, "5000");
    expect(await within(form).findByText(/whole number from 1 to 1000/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save auto-pause" })).toBeDisabled();
    await userEvent.clear(input);
    await userEvent.type(input, "5");
    await userEvent.click(within(form).getByRole("button", { name: "Save auto-pause" }));
    await waitFor(() => expect(st.calls.patchSub).toEqual([{ pause_after_failures: 5 }]));
    await toast("Auto-pause at 5 failures");
    await waitFor(() => expect(screen.getByLabelText("Pause after failures")).toHaveValue("5"));
    await userEvent.clear(screen.getByLabelText("Pause after failures"));
    await userEvent.click(screen.getByRole("button", { name: "Save auto-pause" }));
    await waitFor(() => expect(st.calls.patchSub.at(-1)).toEqual({ pause_after_failures: 0 }));
  });

  it("retries the deliveries in backoff now, and disables the button when none is waiting", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Retry now (1 waiting)" }));
    await waitFor(() => expect(st.calls.retry).toEqual([{}]));
    await toast("Retry started");
    await waitFor(() => expect(screen.getByRole("button", { name: "Retry now" })).toBeDisabled());
  });
});

describe("Push subscription: signing secret", () => {
  it("rotates with a grace window, shows the new secret once with a store-it-now warning, and keeps it out of the page afterwards", async () => {
    const { mock, st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Rotate secret" }));
    const dialog = await screen.findByRole("alertdialog", { name: /Rotate the signing secret of hook/ });
    expect(dialog).toHaveTextContent(/shown once/);
    await pick(within(dialog).getByLabelText("Grace window"), String(604_800_000));
    await userEvent.click(within(dialog).getByRole("button", { name: "Rotate secret" }));
    const banner = await screen.findByRole("status", { name: "New signing secret for hook" });
    expect(within(banner).getByTestId("secret-once")).toHaveTextContent("fbwh_new_secret_1");
    expect(banner).toHaveTextContent(/Store it now/);
    expect(banner).toHaveTextContent(/also signs until/);
    expect(within(banner).getByRole("button", { name: "Copy secret" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/queues/subscriptions/{sub}/secret/rotate"]).toEqual([{ grace_ms: 604_800_000 }]);
    expect(st.subs.find((x) => x.name === "hook")!.push_secret_previous_expires_at).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "I have copied it" }));
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("fbwh_new_secret_1");
    // The previous secret is now shown as still signing.
    expect(await screen.findByText(/also signs until/)).toBeInTheDocument();
  });

  it("expires the previous secret now after a confirmation", async () => {
    const { st } = setup({ push_secret_previous_expires_at: "2026-11-10T10:00:00Z" });
    renderStudio("/queues/s/hook");
    expect(await screen.findByText(/While both sign/)).toBeInTheDocument();
    await userEvent.click(await screen.findByRole("button", { name: "Expire previous now" }));
    await confirmDialog(/Stop signing with the previous secret/, "Expire previous now");
    await waitFor(() => expect(st.calls.expire).toBe(1));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Expire previous now" })).not.toBeInTheDocument());
  });

  it("explains that a function subscription has no secret to rotate", async () => {
    setup({ push: { kind: "function", endpoint: "notify" } });
    renderStudio("/queues/s/hook");
    expect(await screen.findByText(/nothing is signed, so there is no secret to rotate/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Rotate secret" })).not.toBeInTheDocument();
  });
});

describe("Push subscription: attempt log", () => {
  it("lists attempts newest first with outcome, status, latency and error class", async () => {
    setup();
    renderStudio("/queues/s/hook");
    const table = await screen.findByRole("table", { name: "Attempts" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    expect(within(rows[0]!).getByText("test-3")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("test")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("503")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("http_status")).toBeInTheDocument();
    expect(within(rows[2]!).getByText("120 ms")).toBeInTheDocument();
  });

  it("filters by outcome, message id and test sends, sending them to the server", async () => {
    const { mock } = setup();
    renderStudio("/queues/s/hook");
    await screen.findByRole("table", { name: "Attempts" });
    await pick(screen.getByLabelText("Outcome"), "failure");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Attempts" })).getAllByRole("row")).toHaveLength(2));
    expect(mock.queries.some((q, i) => mock.calls[i]!.endsWith("/attempts") && q.get("outcome") === "failure")).toBe(true);
    await pick(screen.getByLabelText("Outcome"), "");
    await pick(screen.getByLabelText("Kind"), "true");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Attempts" })).getAllByRole("row")).toHaveLength(2));
    expect(mock.queries.some((q, i) => mock.calls[i]!.endsWith("/attempts") && q.get("test") === "true")).toBe(true);
    await pick(screen.getByLabelText("Kind"), "");
    await userEvent.type(screen.getByLabelText("Message id"), "msg_a");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Attempts" })).getAllByRole("row")).toHaveLength(2));
    expect(mock.queries.some((q, i) => mock.calls[i]!.endsWith("/attempts") && q.get("message_id") === "msg_a")).toBe(true);
    await userEvent.type(screen.getByLabelText("Message id"), "zzz");
    expect(await screen.findByText("No attempts match")).toBeInTheDocument();
  });

  it("pages with Load more", async () => {
    const { st } = setup();
    for (let i = 10; i < 70; i++) st.attempts.push(pushAttempt(i));
    renderStudio("/queues/s/hook");
    await screen.findByRole("table", { name: "Attempts" });
    expect(within(screen.getByRole("table", { name: "Attempts" })).getAllByRole("row")).toHaveLength(51);
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Attempts" })).getAllByRole("row")).toHaveLength(64));
  });

  it("opens an attempt drawer: status, latency, error class, masked request headers, response headers and a truncated body", async () => {
    setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Show attempt 2" }));
    const drawer = await screen.findByRole("dialog", { name: "Attempt 2" });
    expect(within(drawer).getByText("503")).toBeInTheDocument();
    expect(within(drawer).getByText("http_status")).toBeInTheDocument();
    expect(within(drawer).getByText("120 ms")).toBeInTheDocument();
    expect(within(drawer).getByRole("alert")).toHaveTextContent("endpoint answered 503");
    const req = within(drawer).getByRole("table", { name: "Request headers table" });
    expect(req).toHaveTextContent("t=1731578400,v1=********,v1=********");
    expect(req).not.toHaveTextContent("deadbeefcafe");
    expect(req).not.toHaveTextContent("leaked");
    expect(within(drawer).getByRole("table", { name: "Response headers table" })).toHaveTextContent("text/plain");
    expect(within(drawer).getByLabelText("Response body text")).toHaveTextContent("unavailable");
    expect(within(drawer).getByRole("note")).toHaveTextContent(/Truncated.*9,000 bytes/);
  });

  it("retries one failed message now from its drawer", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Show attempt 2" }));
    const drawer = await screen.findByRole("dialog", { name: "Attempt 2" });
    await userEvent.click(within(drawer).getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(st.calls.retry).toEqual([{ message_ids: ["msg_b"] }]));
    await toast("Retry started");
  });

  it("sends a test event and shows the resulting attempt, a failure with its error", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Send test event" }));
    const ok = await screen.findByRole("dialog", { name: /Attempt 1001/ });
    expect(ok).toHaveTextContent("Test send");
    expect(within(ok).getByText("200")).toBeInTheDocument();
    await toast(/Test event accepted/);
    expect(st.calls.test).toBe(1);
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Attempt 1001/ })).not.toBeInTheDocument());

    st.testResult = "failure";
    await userEvent.click(screen.getByRole("button", { name: "Send test event" }));
    const bad = await screen.findByRole("dialog", { name: /Attempt 1002/ });
    expect(within(bad).getByRole("alert")).toHaveTextContent("endpoint answered 500");
    expect(within(bad).getByText("http_status")).toBeInTheDocument();
    // Test sends are rows of the log too, marked as tests, and have no Retry now.
    expect(within(bad).queryByRole("button", { name: "Retry now" })).not.toBeInTheDocument();
  });

  it("explains a rate-limited test send", async () => {
    const { st } = setup();
    st.testResult = "rate_limited";
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Send test event" }));
    expect(await screen.findByText("Too many requests")).toBeInTheDocument();
  });
});

describe("Dead letters: redrive", () => {
  it("redrives the selected dead letters after a confirmation and reports a count; a redriven one cannot be selected again", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    const list = await screen.findByRole("list", { name: "Dead letters" });
    expect(within(list).getByRole("checkbox", { name: "Select dead letter 12" })).toHaveAttribute("aria-disabled", "true");
    expect(within(list).getByText(/redriven/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Redrive selected" })).toBeDisabled();
    await userEvent.click(within(list).getByRole("checkbox", { name: "Select dead letter 11" }));
    await userEvent.click(screen.getByRole("button", { name: "Redrive selected (1)" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Redrive 1 dead letter?" });
    expect(dialog).toHaveTextContent(/fresh attempt counter/);
    expect(st.calls.redrive).toEqual([]);
    await userEvent.click(within(dialog).getByRole("button", { name: "Redrive selected" }));
    await waitFor(() => expect(st.calls.redrive).toEqual([{ ids: [11] }]));
    expect(await screen.findByText("Redriven 1 dead letter.")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("checkbox", { name: "Select dead letter 11" })).toHaveAttribute("aria-disabled", "true"));
  });

  it("does nothing when the confirmation is cancelled", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("checkbox", { name: "Select dead letter 11" }));
    await userEvent.click(screen.getByRole("button", { name: "Redrive selected (1)" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(st.calls.redrive).toEqual([]);
  });

  it("redrives all with a since time and an attribute filter, and refuses a filter that is not JSON", async () => {
    const { st } = setup();
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Redrive all…" }));
    const dialog = await screen.findByRole("alertdialog", { name: /Redrive all dead letters of hook/ });
    await userEvent.type(within(dialog).getByLabelText(/^Dead-lettered since/), "2026-11-09T08:00");
    await userEvent.type(within(dialog).getByLabelText(/^Attribute filter/), "{{nope");
    expect(await within(dialog).findByText("That is not valid JSON.")).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Redrive all" }));
    expect(await within(dialog).findByRole("alert")).toBeInTheDocument();
    expect(st.calls.redrive).toEqual([]);
    const box = within(dialog).getByLabelText(/^Attribute filter/);
    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"kind": "rush"}');
    await userEvent.click(within(dialog).getByRole("button", { name: "Redrive all" }));
    await waitFor(() => expect(st.calls.redrive).toHaveLength(1));
    expect(st.calls.redrive[0]).toEqual({ all: true, since: new Date("2026-11-09T08:00").toISOString(), filter: { kind: "rush" } });
    expect(await screen.findByText("Redriven 1 dead letter.")).toBeInTheDocument();
  });

  it("shows an explained error when the server refuses the redrive", async () => {
    const { mock } = setup();
    mock.custom.unshift({ method: "POST", template: "/api/v1/queues/subscriptions/{sub}/redrive", handler: () => envelope(400, "invalid_redrive", "bad") });
    renderStudio("/queues/s/hook");
    await userEvent.click(await screen.findByRole("button", { name: "Redrive all…" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Redrive all" }));
    expect(await within(dialog).findByText("That redrive request is not valid")).toBeInTheDocument();
  });
});

describe("Topics: description and example", () => {
  it("saves a topic's description and example and removes the example again", async () => {
    const { st } = setup();
    renderStudio("/queues/t/orders");
    const form = await screen.findByRole("form", { name: "Catalog entry" });
    await userEvent.type(within(form).getByLabelText("Description"), "An order was placed");
    await userEvent.click(within(form).getByLabelText(/^Example/));
    await userEvent.paste('{"order_id": "o1"}');
    await userEvent.click(within(form).getByRole("button", { name: "Save catalog entry" }));
    await waitFor(() => expect(st.calls.patchTopic).toEqual([{ description: "An order was placed", example: { order_id: "o1" } }]));
    await toast("Catalog entry saved");
    await waitFor(() => expect(screen.getByLabelText("Description")).toHaveValue("An order was placed"));
    await userEvent.clear(screen.getByLabelText(/^Example/));
    await userEvent.click(screen.getByRole("button", { name: "Save catalog entry" }));
    await waitFor(() => expect(st.calls.patchTopic.at(-1)).toEqual({ description: "An order was placed", clear_example: true }));
  });

  it("checks the example is JSON, and shows the server's refusal of one that breaks the schema", async () => {
    const { st } = setup();
    st.topics.find((t) => t.name === "orders")!.schema = { type: "object" };
    renderStudio("/queues/t/orders");
    const form = await screen.findByRole("form", { name: "Catalog entry" });
    await userEvent.click(within(form).getByLabelText(/^Example/));
    await userEvent.paste("{broken");
    expect(await within(form).findByText("The example is not valid JSON.")).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save catalog entry" })).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Example/));
    await userEvent.paste("42");
    await userEvent.click(within(form).getByRole("button", { name: "Save catalog entry" }));
    expect(await screen.findByText("The example does not fit the topic's schema")).toBeInTheDocument();
  });

  it("creates a topic with a description and an example", async () => {
    const { mock } = setup();
    renderStudio("/queues?new=1");
    const form = await sheet("New topic");
    await userEvent.type(within(form).getByLabelText(/^Name/), "payments");
    await userEvent.type(within(form).getByLabelText("Description"), "A payment settled");
    await userEvent.click(within(form).getByLabelText(/^Example/));
    await userEvent.paste('{"amount": 5}');
    await userEvent.click(within(form).getByRole("button", { name: "Create topic" }));
    await toast("Topic payments created");
    expect(mock.bodies["post /api/v1/queues/topics"]).toEqual([{ name: "payments", description: "A payment settled", example: { amount: 5 } }]);
  });
});

describe("Event catalog", () => {
  function catalog() {
    const { st } = setup();
    st.topics.push(topic("audit", { source: { collection: "users", ops: ["create", "update"] } }));
    st.catalog = [
      { topic: "orders", description: "An order was placed", data_schema: { type: "object", properties: { order_id: { type: "string" } } }, example: { order_id: "o1" }, example_source: "topic", source: null, event_types: [] },
      {
        topic: "users-changes", tenant: null, description: "Record changes of users", data_schema: null, example: {}, example_source: "schema", source: { collection: "users", ops: ["create", "update"] },
        event_types: [
          { type: "created", op: "create", description: "A user was created", filter: { "forgebase.op": "create" }, data_schema: { type: "object", properties: { record: { type: "object" } } }, example: { record: { email: "a@b.c" } } },
          { type: "updated", op: "update", description: "A user was changed", filter: { "forgebase.op": "update" }, data_schema: { type: "object" }, example: { record: { email: "d@e.f" } } },
        ],
      },
      { topic: "bare", description: null, data_schema: null, example: {}, example_source: "none", source: null, event_types: [] },
    ];
    return st;
  }

  it("lists topics with description, schema and example, and event types per record source", async () => {
    catalog();
    renderStudio("/queues/catalog");
    const orders = await screen.findByRole("region", { name: "Topic orders" });
    expect(orders).toHaveTextContent("An order was placed");
    expect(within(orders).getByLabelText("orders example")).toHaveTextContent('"order_id": "o1"');
    expect(within(orders).getByText("orders data schema")).toBeInTheDocument();
    const users = screen.getByRole("region", { name: "Topic users-changes" });
    expect(within(users).getByText("records of users")).toBeInTheDocument();
    const events = within(users).getByRole("list", { name: "Event types of users-changes" });
    expect(within(events).getByLabelText("Event record.created")).toHaveTextContent("A user was created");
    expect(within(events).getByLabelText("record.updated filter")).toHaveTextContent('{"forgebase.op":"update"}');
    expect(within(events).getByLabelText("record.created example")).toHaveTextContent("a@b.c");
    const bare = screen.getByRole("region", { name: "Topic bare" });
    expect(bare).toHaveTextContent(/Nothing describes this topic's data yet/);
    expect(within(bare).queryByLabelText("bare example")).not.toBeInTheDocument();
  });

  it("copies a curl and a `forgebase queues listen` command for a topic and for one event type", async () => {
    catalog();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    renderStudio("/queues/catalog");
    const orders = await screen.findByRole("region", { name: "Topic orders" });
    await userEvent.click(within(orders).getByRole("button", { name: "Copy curl" }));
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining("/api/v1/queues/topics/orders/publish"));
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining(`{"messages":[{"data":{"order_id":"o1"}}]}`));
    await userEvent.click(within(orders).getByRole("button", { name: "Copy listen command" }));
    expect(writeText).toHaveBeenLastCalledWith("forgebase queues listen 'orders' --forward-to http://localhost:3000/webhooks");

    const created = within(screen.getByRole("region", { name: "Topic users-changes" })).getByLabelText("Event record.created");
    await userEvent.click(within(created).getByRole("button", { name: "Copy listen command" }));
    expect(writeText).toHaveBeenLastCalledWith(`forgebase queues listen 'users-changes' --filter '{"forgebase.op":"create"}' --forward-to http://localhost:3000/webhooks`);
    await userEvent.click(within(created).getByRole("button", { name: "Copy curl" }));
    expect(writeText).toHaveBeenLastCalledWith(expect.stringContaining(`"filter":{"forgebase.op":"create"}`));
  });

  it("searches topics, and says when none match or there are none", async () => {
    const st = catalog();
    renderStudio("/queues/catalog");
    await screen.findByRole("region", { name: "Topic orders" });
    await userEvent.type(screen.getByLabelText("Search the catalog"), "users");
    await waitFor(() => expect(screen.queryByRole("region", { name: "Topic orders" })).not.toBeInTheDocument());
    expect(screen.getByRole("region", { name: "Topic users-changes" })).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Search the catalog"), "zzz");
    expect(await screen.findByText("No topic matches")).toBeInTheDocument();
    st.catalog = [];
  });

  it("is a tab of the Queues studio", async () => {
    catalog();
    renderStudio("/queues");
    const tabs = await screen.findByRole("tablist", { name: "Queues sections" });
    expect(within(tabs).getAllByRole("tab").map((t) => t.textContent)).toEqual(["Topics", "Subscriptions", "Event catalog", "Pub/Sub inspector"]);
    await userEvent.click(within(tabs).getByRole("tab", { name: "Event catalog" }));
    expect(await screen.findByRole("region", { name: "Topic orders" })).toBeInTheDocument();
  });
});
