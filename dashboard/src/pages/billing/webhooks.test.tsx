import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installBilling } from "../../test/billingMock";
import { pick } from "../../test/pick";
import { toast } from "../../test/dataStudioMock";
import { GROUPS } from "./groups";
import { bodyView, isRefused, resultWords, touchedObjects } from "./webhookUtil";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installBilling(mock) };
}
const rows = () => within(screen.getByRole("table", { name: "Webhook deliveries" })).getAllByRole("row").slice(1);
const open = async (id: string) => {
  await userEvent.click(await screen.findByRole("button", { name: `Show delivery ${id}` }));
  return screen.findByRole("dialog", { name: `Delivery ${id}` });
};
const calls = (mock: MockApi, suffix: string) => mock.queries.filter((_q, i) => mock.calls[i]!.endsWith(suffix));

describe("webhook helpers", () => {
  it("shows a body as JSON, text or base64, and none when it was not kept", () => {
    expect(bodyView({ body: '{"a":1}', body_encoding: "utf8", body_stored: true, body_size: 7 })).toEqual({ kind: "json", text: '{\n  "a": 1\n}' });
    expect(bodyView({ body: "a=1&b=2", body_encoding: "utf8", body_stored: true, body_size: 7 })).toEqual({ kind: "text", text: "a=1&b=2" });
    expect(bodyView({ body: "AAEC/w==", body_encoding: "base64", body_stored: true, body_size: 4 })).toEqual({ kind: "base64", text: "AAEC/w==", bytes: 4 });
    expect(bodyView({ body: null, body_encoding: null, body_stored: false, body_size: 9 }).kind).toBe("none");
    expect(bodyView({ body: "x", body_encoding: "utf8", body_stored: false, body_size: 1 }).kind).toBe("none");
  });

  it("lists the touched objects with where to look at each", () => {
    expect(touchedObjects({ invoice_id: "inv 1", customer_id: "cus1", webhook_event_id: "we1", payment_id: null })).toEqual([
      { key: "webhook_event_id", label: "Webhook event", id: "we1", to: undefined },
      { key: "customer_id", label: "Customer", id: "cus1", to: "/billing/customers" },
      { key: "invoice_id", label: "Invoice", id: "inv 1", to: "/billing/invoices/inv%201" },
    ]);
    expect(touchedObjects({})).toEqual([]);
  });

  it("puts a replay or re-verify answer in plain words", () => {
    expect(resultWords({ result: "applied", detail: "invoice_paid" })).toMatchObject({ tone: "ok", title: "Applied" });
    expect(resultWords({ result: "already_applied" })).toMatchObject({ tone: "info", text: expect.stringContaining("nothing changed") });
    expect(resultWords({ result: "already_ignored" }).title).toBe("Already ignored");
    expect(resultWords({ result: "failed", detail: "db" })).toMatchObject({ tone: "bad", text: expect.stringContaining("(db)") });
    expect(resultWords({ result: "unverified", detail: "signature_mismatch" })).toMatchObject({ tone: "warn", title: "Still does not verify" });
    expect(resultWords({ result: "ignored" }).tone).toBe("info");
    expect(isRefused({ verification: "rejected" })).toBe(true);
    expect(isRefused({ verification: "verified" })).toBe(false);
  });

  it("puts the page in the Settings group, next to the providers", () => {
    expect(GROUPS.find((g) => g.id === "settings")!.pages.map((p) => p.to)).toEqual(["/billing/settings", "/billing/webhooks"]);
  });
});

describe("Billing: webhook deliveries", () => {
  it("lists deliveries with an outcome badge each, newest first, and a count", async () => {
    setup();
    renderStudio("/billing/webhooks");
    await screen.findByRole("table", { name: "Webhook deliveries" });
    expect(rows()).toHaveLength(6);
    expect(within(rows()[0]!).getByText("applied")).toBeInTheDocument();
    expect(within(rows()[1]!).getByText("signature invalid")).toBeInTheDocument();
    expect(within(rows()[2]!).getByText("duplicate")).toBeInTheDocument();
    expect(within(rows()[2]!).getByText("rzp_live_1")).toBeInTheDocument();
    expect(within(rows()[3]!).getByText("failed")).toBeInTheDocument();
    expect(within(rows()[4]!).getByText("rejected")).toBeInTheDocument();
    expect(within(rows()[0]!).getByText("invoice.paid")).toBeInTheDocument();
    expect(within(rows()[1]!).getByText("signature_mismatch")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Webhook deliveries" })).toHaveAttribute("aria-selected", "true");
  });

  it("filters by provider, outcome, event type and time range, and sends each to the server", async () => {
    const { mock } = setup();
    renderStudio("/billing/webhooks");
    await screen.findByRole("table", { name: "Webhook deliveries" });
    await pick(screen.getByLabelText("Provider"), "razorpay");
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(calls(mock, "/webhook-deliveries").at(-1)!.get("provider")).toBe("razorpay");
    await pick(screen.getByLabelText("Provider"), "");
    await pick(screen.getByLabelText("Outcome"), "signature_invalid");
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(calls(mock, "/webhook-deliveries").at(-1)!.get("outcome")).toBe("signature_invalid");
    await pick(screen.getByLabelText("Outcome"), "");
    await userEvent.type(screen.getByLabelText("Event type"), "payment.captured");
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(calls(mock, "/webhook-deliveries").at(-1)!.get("event_type")).toBe("payment.captured");
    await userEvent.clear(screen.getByLabelText("Event type"));
    await userEvent.type(screen.getByLabelText("From"), "2026-11-02T00:00");
    expect(await screen.findByText("No deliveries match")).toBeInTheDocument();
    expect(calls(mock, "/webhook-deliveries").at(-1)!.get("from")).toBe(new Date("2026-11-02T00:00").toISOString());
    await userEvent.clear(screen.getByLabelText("From"));
    await userEvent.type(screen.getByLabelText("To"), "2026-11-02T00:00");
    await waitFor(() => expect(rows()).toHaveLength(6));
    expect(calls(mock, "/webhook-deliveries").at(-1)!.get("to")).toBe(new Date("2026-11-02T00:00").toISOString());
  });

  it("pages through the deliveries", async () => {
    const { st } = setup();
    for (let i = 10; i < 130; i++) st.deliveries.push({ ...st.deliveries[0]!, id: `whd_${i}` });
    renderStudio("/billing/webhooks");
    await screen.findByRole("table", { name: "Webhook deliveries" });
    expect(rows()).toHaveLength(50);
    await userEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Show delivery whd_54" })).toBeInTheDocument());
  });

  it("says what to do when nothing has arrived yet", async () => {
    const { st } = setup();
    st.deliveries.length = 0;
    renderStudio("/billing/webhooks");
    expect(await screen.findByText("No webhook deliveries yet")).toBeInTheDocument();
  });

  it("opens a verified delivery: facts, allowlisted headers, a pretty body and links to the billing objects it touched", async () => {
    setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_1");
    expect(within(drawer).getByText("invoice.paid", { selector: "dd" })).toBeInTheDocument();
    expect(within(drawer).getByText("203.0.113.7")).toBeInTheDocument();
    expect(within(drawer).getByRole("table", { name: "Stored headers" })).toHaveTextContent("stripe-signature");
    expect(within(drawer).getByRole("table", { name: "Stored headers" })).toHaveTextContent("content-type");
    expect(within(drawer).getByLabelText("Body")).toHaveTextContent(/"type": "invoice.paid"/);
    const objects = within(drawer).getByRole("list", { name: "Touched objects" });
    expect(within(objects).getByRole("link", { name: "Invoice inv1" })).toHaveAttribute("href", "/billing/invoices/inv1");
    expect(within(objects).getByRole("link", { name: "Customer cus1" })).toHaveAttribute("href", "/billing/customers");
    expect(within(objects).getByText("we1")).toBeInTheDocument();
    expect(within(drawer).getByRole("button", { name: "Replay" })).toBeInTheDocument();
    expect(within(drawer).queryByRole("button", { name: "Re-verify" })).not.toBeInTheDocument();
  });

  it("shows a binary body as base64 with its byte count", async () => {
    setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_4");
    expect(within(drawer).getByText(/Binary body, shown as base64 \(4 bytes\)/)).toBeInTheDocument();
    expect(within(drawer).getByLabelText("Body (base64)")).toHaveTextContent("AAEC/w==");
    expect(within(drawer).getByRole("button", { name: "Copy body" })).toBeInTheDocument();
  });

  it("replays a verified delivery after a confirmation and shows the result in words and as a toast", async () => {
    const { st } = setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_1");
    await userEvent.click(within(drawer).getByRole("button", { name: "Replay" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Replay this delivery?" });
    expect(st.calls.replay).toEqual([]);
    await userEvent.click(within(confirm).getByRole("button", { name: "Replay" }));
    await waitFor(() => expect(st.calls.replay).toEqual(["whd_1"]));
    expect(await within(drawer).findByText("The event was processed now and changed billing state (invoice_paid).")).toBeInTheDocument();
    await toast("Replay: Applied");
    await waitFor(() => expect(within(drawer).getByText("Replays").nextSibling).toHaveTextContent("1"));
  });

  it("explains an already-applied event and a failed replay", async () => {
    const { st } = setup();
    st.replayAs = "already_applied";
    renderStudio("/billing/webhooks");
    let drawer = await open("whd_1");
    await userEvent.click(within(drawer).getByRole("button", { name: "Replay" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Replay" }));
    expect(await within(drawer).findByText("This event was handled before, so nothing changed.")).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delivery whd_1" })).not.toBeInTheDocument());

    st.replayAs = "failed";
    drawer = await open("whd_3");
    await userEvent.click(within(drawer).getByRole("button", { name: "Replay" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Replay" }));
    expect(await within(drawer).findByText(/Processing failed again/)).toBeInTheDocument();
    await toast("Replay: Failed");
  });

  it("offers Re-verify, not Replay, for a refused delivery, and says why it was refused", async () => {
    setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_2");
    expect(within(drawer).getByRole("button", { name: "Re-verify" })).toBeInTheDocument();
    expect(within(drawer).queryByRole("button", { name: "Replay" })).not.toBeInTheDocument();
    expect(within(drawer).getByText("Refused: signature invalid")).toBeInTheDocument();
    expect(within(drawer).getByText(/did not match any webhook secret/)).toBeInTheDocument();
    expect(within(drawer).getByText(/Reason: signature_mismatch/)).toBeInTheDocument();
  });

  it("re-verifies a refused delivery with the current secrets: it now verifies and is applied", async () => {
    const { st } = setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_2");
    await userEvent.click(within(drawer).getByRole("button", { name: "Re-verify" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Re-verify this delivery?" });
    await userEvent.click(within(confirm).getByRole("button", { name: "Re-verify" }));
    await waitFor(() => expect(st.calls.reverify).toEqual(["whd_2"]));
    expect(await within(drawer).findByText(/changed billing state/)).toBeInTheDocument();
    await toast("Re-verify: Applied");
    // The drawer now treats it as verified: Replay is offered.
    expect(await within(drawer).findByRole("button", { name: "Replay" })).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delivery whd_2" })).not.toBeInTheDocument());
    await waitFor(() => expect(rows()[1]).toHaveTextContent("applied"));
  });

  it("says when a re-verify still does not verify", async () => {
    const { st } = setup();
    st.reverifyAs = "unverified";
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_2");
    await userEvent.click(within(drawer).getByRole("button", { name: "Re-verify" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Re-verify" }));
    const note = await within(drawer).findByText(/does not verify with the current webhook secrets/);
    expect(note).toHaveTextContent("signature_mismatch");
    expect(within(drawer).getByRole("button", { name: "Re-verify" })).toBeInTheDocument();
    await toast("Re-verify: Still does not verify");
  });

  it("explains a delivery whose body was not kept: the drawer says so, and re-verify is answered with a 409 in plain words", async () => {
    setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_5");
    expect(within(drawer).getByText(/The body was not kept/)).toBeInTheDocument();
    await userEvent.click(within(drawer).getByRole("button", { name: "Re-verify" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Re-verify" }));
    expect(await within(drawer).findByText("The body of this delivery was not kept")).toBeInTheDocument();
    expect(within(drawer).getByText(/budget, and bodies over 256 KiB are cut/)).toBeInTheDocument();
  });

  it("explains a Google Play delivery that cannot be verified again", async () => {
    setup();
    renderStudio("/billing/webhooks");
    const drawer = await open("whd_6");
    await userEvent.click(within(drawer).getByRole("button", { name: "Re-verify" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Re-verify" }));
    expect(await within(drawer).findByText("This delivery cannot be verified again")).toBeInTheDocument();
    expect(within(drawer).getByText(/Pub\/Sub sends unacknowledged messages again/)).toBeInTheDocument();
  });

  it.each([
    ["POST /api/v1/billing/webhook-deliveries/{id}/replay", "whd_1", "Replay", "delivery_not_verified", "This delivery was refused, so it cannot be replayed"],
    ["POST /api/v1/billing/webhook-deliveries/{id}/reverify", "whd_2", "Re-verify", "delivery_already_verified", "This delivery already verified"],
    ["POST /api/v1/billing/webhook-deliveries/{id}/reverify", "whd_2", "Re-verify", "reverify_in_progress", "Another re-verify of this delivery is running"],
  ])("shows the 409 %s as an explained error (%s %s)", async (route, id, button, code, title) => {
    const { mock } = setup();
    const [method, template] = route.split(" ") as [string, string];
    mock.custom.unshift({ method, template, handler: () => envelope(409, code, code) });
    renderStudio("/billing/webhooks");
    const drawer = await open(id);
    await userEvent.click(within(drawer).getByRole("button", { name: button }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: button }));
    expect(await within(drawer).findByText(title)).toBeInTheDocument();
  });

  it("says billing is disabled when its routes are off", async () => {
    const mock = signedIn(new MockApi().install());
    installBilling(mock, { disabled: true });
    renderStudio("/billing/webhooks");
    expect(await screen.findByRole("status", { name: "Billing disabled" }, { timeout: 5000 })).toBeInTheDocument();
  });
});
