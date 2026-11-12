import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { pick } from "../../test/pick";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installBilling } from "../../test/billingMock";
import { sheet, toast } from "../../test/dataStudioMock";
import { GROUPS, groupOf } from "./groups";

// Sign-in answers /api/auth/me; a test sets `adminScope.value` to see Billing as a realm administrator.
const adminScope = vi.hoisted(() => ({ value: undefined as undefined | { realm: string } }));
vi.mock("../../auth/session", async (orig) => {
  const m = await orig<typeof import("../../auth/session")>();
  return { ...m, useOptionalSession: () => (adminScope.value ? ({ adminScope: adminScope.value } as ReturnType<typeof m.useOptionalSession>) : m.useOptionalSession()) };
});

let current: MockApi | undefined;
afterEach(() => {
  // Every billing request and response matched the OpenAPI document.
  expect(current?.problems ?? []).toEqual([]);
  current = undefined;
  adminScope.value = undefined;
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup(opts: { disabled?: boolean } = {}) {
  const mock = signedIn(new MockApi().install());
  current = mock;
  return { mock, st: installBilling(mock, opts) };
}
const rows = (name: string) => within(screen.getByRole("table", { name })).getAllByRole("row");
const lastBody = (mock: MockApi, key: string) => mock.bodies[key]!.at(-1) as Record<string, unknown>;

describe("Billing shell", () => {
  it("shows a clear disabled state when the billing routes answer 404", async () => {
    setup({ disabled: true });
    renderStudio("/billing");
    expect(await screen.findByRole("status", { name: "Billing disabled" }, { timeout: 5000 })).toHaveTextContent(/on by default/);
    expect(screen.queryByRole("tablist", { name: "Billing groups" })).not.toBeInTheDocument();
  });

  it("offers four groups, and every page in its group's second row", async () => {
    setup();
    renderStudio("/billing");
    const groups = await screen.findByRole("tablist", { name: "Billing groups" });
    expect(within(groups).getAllByRole("tab").map((a) => a.textContent)).toEqual(["Catalog", "Customers", "Money", "Settings"]);
    expect(within(groups).getByRole("tab", { name: "Money" })).toHaveAttribute("aria-selected", "true");
    const pages = screen.getByRole("tablist", { name: "Money pages" });
    expect(within(pages).getAllByRole("tab").map((a) => a.textContent)).toEqual(["Revenue", "Invoices", "Payments", "Credits", "Dunning"]);
  });
});

describe("Billing groups", () => {
  it("puts every route in one group, and the 13 pages in four", () => {
    expect(groupOf("/billing").id).toBe("money");
    expect(groupOf("/billing/invoices/inv1").id).toBe("money");
    expect(groupOf("/billing/coupons").id).toBe("catalog");
    expect(groupOf("/billing/stores").id).toBe("catalog");
    expect(groupOf("/billing/usage").id).toBe("customers");
    expect(groupOf("/billing/settings").id).toBe("settings");
    expect(GROUPS.flatMap((g) => g.pages)).toHaveLength(13);
  });
  it("moves between groups and pages", async () => {
    setup();
    renderStudio("/billing");
    const user = userEvent.setup();
    const groups = await screen.findByRole("tablist", { name: "Billing groups" });
    await user.click(within(groups).getByRole("tab", { name: "Catalog" }));
    const pages = await screen.findByRole("tablist", { name: "Catalog pages" });
    await user.click(within(pages).getByRole("tab", { name: "Coupons" }));
    await waitFor(() => expect(within(screen.getByRole("tablist", { name: "Catalog pages" })).getByRole("tab", { name: "Coupons" })).toHaveAttribute("aria-selected", "true"));
    await user.click(within(screen.getByRole("tablist", { name: "Billing groups" })).getByRole("tab", { name: "Settings" }));
    await waitFor(() => expect(screen.getByRole("tab", { name: "Settings" })).toHaveAttribute("aria-selected", "true"));
  });
  it("shows Billing to a platform administrator only", async () => {
    setup();
    adminScope.value = { realm: "acme" };
    renderStudio("/billing");
    expect(await screen.findByRole("region", { name: "Billing is for platform administrators" })).toBeInTheDocument();
    expect(screen.queryByRole("tablist", { name: "Billing groups" })).not.toBeInTheDocument();
  });
});

describe("Revenue", () => {
  it("shows MRR, ARR, churn and revenue by currency", async () => {
    setup();
    renderStudio("/billing");
    const recurring = await screen.findByRole("region", { name: "Recurring revenue" });
    expect(recurring).toHaveTextContent("$1,299.00");
    expect(recurring).toHaveTextContent("$15,588.00");
    expect(recurring).toHaveTextContent("¥500,000");
    const metrics = screen.getByLabelText("Subscription metrics");
    expect(metrics).toHaveTextContent("Churn rate10%");
    expect(metrics).toHaveTextContent("Failed payments2");
    expect(within(screen.getByRole("table", { name: "Revenue" })).getAllByText("¥90,000")).toHaveLength(2);
  });
});

describe("Catalog", () => {
  it("lists products with recurring, metered, tiered, one-time and multi-currency prices", async () => {
    setup();
    renderStudio("/billing/catalog");
    const table = await screen.findByRole("table", { name: "Products" });
    expect(within(table).getByText("$29.00 / month", { exact: false })).toBeInTheDocument();
    expect(within(table).getAllByText("€27.00").length).toBeGreaterThan(0);
    expect(within(table).getByText(/graduated tiers \(2\)/)).toBeInTheDocument();
    expect(within(table).getByText(/metered api_calls/)).toBeInTheDocument();
    expect(within(table).getByText("¥1,500")).toBeInTheDocument(); // JPY has no minor unit: 1500 is 1,500 yen
  });

  it("creates a product and a tiered multi-currency price in minor units", async () => {
    const { mock } = setup();
    renderStudio("/billing/catalog");
    await screen.findByRole("table", { name: "Products" });
    await userEvent.click(screen.getByRole("button", { name: "New product" }));
    const pf = await sheet("New product");
    await userEvent.type(within(pf).getByLabelText("Product key"), "scale");
    await userEvent.type(within(pf).getByLabelText("Product name"), "Scale");
    await userEvent.click(within(pf).getByRole("button", { name: "Save product" }));
    await toast(/Product Scale created/);
    expect(lastBody(mock, "put /api/v1/billing/products/{key}")).toMatchObject({ name: "Scale", kind: "plan", active: true });

    await userEvent.click(screen.getByRole("button", { name: "New price" }));
    const form = await sheet("New price");
    await userEvent.type(within(form).getByLabelText("Price key"), "scale_usage");
    await pick(within(form).getByLabelText("Price type"), "metered");
    await userEvent.type(within(form).getByLabelText("Metered feature"), "api_calls");
    await pick(within(form).getByLabelText("Scheme"), "graduated");
    await userEvent.type(within(form).getByLabelText("Unit amount"), "0.10");
    await userEvent.click(within(form).getByLabelText(/^Tiers/));
    await userEvent.paste("100, 0.10, 0\ninf, 0.05, 0");
    await userEvent.click(within(form).getByLabelText(/^Other currencies/));
    await userEvent.paste("EUR=0.09");
    await userEvent.click(within(form).getByRole("button", { name: "Save price" }));
    await toast(/Price scale_usage created/);
    expect(lastBody(mock, "put /api/v1/billing/prices/{key}")).toMatchObject({
      type: "metered", scheme: "graduated", currency: "USD", unit_amount: 10, feature: "api_calls",
      tiers: [{ up_to: 100, unit_amount: 10, flat_amount: 0 }, { up_to: null, unit_amount: 5, flat_amount: 0 }],
      currency_options: { EUR: { unit_amount: 9 } },
    });
  });

  it("archives a product and a price through the archive endpoints", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/catalog");
    await screen.findByRole("table", { name: "Products" });
    await userEvent.click(screen.getByRole("button", { name: "Archive price team_month" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm archive price team_month" }));
    await waitFor(() => expect(st.prices.find((p) => p.key === "team_month")!.active).toBe(false));
    await userEvent.click(screen.getByRole("button", { name: "Archive product team" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm archive product team" }));
    await waitFor(() => expect(st.products.find((p) => p.key === "team")!.active).toBe(false));
    expect(mock.calls).toContain("post /api/v1/billing/products/team/archive");
    expect(mock.calls).not.toContain("put /api/v1/billing/products/team");
    expect(screen.queryByRole("button", { name: "Archive product team" })).not.toBeInTheDocument();
  });

  it("rejects a malformed amount before calling the server", async () => {
    const { mock } = setup();
    renderStudio("/billing/catalog");
    await screen.findByRole("table", { name: "Products" });
    await userEvent.click(screen.getByRole("button", { name: "New price" }));
    const form = await sheet("New price");
    await userEvent.type(within(form).getByLabelText("Price key"), "bad");
    await userEvent.type(within(form).getByLabelText("Unit amount"), "1.234");
    expect(within(form).getByText("1.234 is not a valid USD amount")).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save price" })).toBeDisabled();
    expect(mock.calls).not.toContain("put /api/v1/billing/prices/bad");
  });

  it("lists Entitlements features, plans, add-ons and usage packs", async () => {
    setup();
    renderStudio("/billing/catalog");
    expect(await screen.findByText("1,000 api_calls", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "Add-ons" })).toHaveTextContent("SSO");
    expect(screen.getByRole("list", { name: "Features" })).toHaveTextContent("API calls");
  });
});

describe("Customers", () => {
  it("lists users and organizations, filters them and opens a customer with its balance", async () => {
    const { mock } = setup();
    renderStudio("/billing/customers");
    expect(await within(await screen.findByRole("table", { name: "Customers" })).findByText("$123.45")).toBeInTheDocument();
    expect(rows("Customers")).toHaveLength(3);
    await pick(screen.getByLabelText("Type"), "org");
    expect(rows("Customers")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "Open org:acme" }));
    const summary = await screen.findByLabelText("Customer summary");
    await waitFor(() => expect(summary).toHaveTextContent("960"));
    expect(summary).toHaveTextContent("Visa 4242");
    const form = screen.getByRole("form", { name: "Edit customer" });
    await userEvent.clear(within(form).getByLabelText("Billing email"));
    await userEvent.type(within(form).getByLabelText("Billing email"), "billing@acme.test");
    await userEvent.click(within(form).getByRole("button", { name: "Save customer" }));
    await waitFor(() => expect(lastBody(mock, "put /api/v1/billing/customer")).toMatchObject({ subject: "org:acme", email: "billing@acme.test" }));
    expect(await screen.findByRole("list", { name: "Customer subscriptions" })).toHaveTextContent("pro_month×1");
  });
});

describe("Subscriptions", () => {
  it("lists subscriptions and filters by status", async () => {
    setup();
    renderStudio("/billing/subscriptions");
    await screen.findByRole("table", { name: "Subscriptions" });
    expect(rows("Subscriptions")).toHaveLength(3);
    await pick(screen.getByLabelText("Status"), "past_due");
    expect(rows("Subscriptions")).toHaveLength(2);
  });

  it("pauses, resumes, and cancels at period end", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/subscriptions");
    await screen.findByRole("table", { name: "Subscriptions" });
    await userEvent.type(screen.getByLabelText("Subject"), "org:acme");
    await waitFor(() => expect(rows("Subscriptions")).toHaveLength(2));
    await userEvent.click(screen.getByRole("button", { name: "Manage sub1" }));
    await userEvent.click(await screen.findByRole("button", { name: "Pause" }));
    await waitFor(() => expect(st.subscriptions.find((s) => s.id === "sub1")!.status).toBe("paused"));
    expect(mock.calls).toContain("post /api/v1/billing/subscriptions/sub1/pause");
    await userEvent.click(await screen.findByRole("button", { name: "Resume" }));
    await waitFor(() => expect(st.subscriptions.find((s) => s.id === "sub1")!.status).toBe("active"));
    await userEvent.click(await screen.findByRole("button", { name: "Cancel at period end" }));
    await waitFor(() => expect(lastBody(mock, "post /api/v1/billing/subscriptions/{id}/cancel")).toEqual({ at_period_end: true }));
    expect(await screen.findByText("ends at period end")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Keep subscription" })).toBeInTheDocument();
  });

  it("cancels now only after a second confirming click", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/subscriptions");
    await screen.findByRole("table", { name: "Subscriptions" });
    await userEvent.click(screen.getByRole("button", { name: "Manage sub1" }));
    await userEvent.click(await screen.findByRole("button", { name: "Cancel now" }));
    expect(mock.calls).not.toContain("post /api/v1/billing/subscriptions/{id}/cancel");
    await userEvent.click(screen.getByRole("button", { name: "Confirm cancel now" }));
    await waitFor(() => expect(st.subscriptions.find((s) => s.id === "sub1")!.status).toBe("canceled"));
    expect(lastBody(mock, "post /api/v1/billing/subscriptions/{id}/cancel")).toMatchObject({ at_period_end: false });
  });

  it("changes plan with the server proration preview", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/subscriptions");
    await screen.findByRole("table", { name: "Subscriptions" });
    await userEvent.click(screen.getByRole("button", { name: "Manage sub1" }));
    await userEvent.click(await screen.findByRole("button", { name: "Change plan" }));
    const group = screen.getByRole("group", { name: "Change plan of sub1" });
    await pick(within(group).getByLabelText("New price"), "team_month");
    const preview = await within(group).findByLabelText("Proration preview");
    expect(preview).toHaveTextContent(/Unused credit -\$14\.50, new charge \$49\.50, net \$35\.00/);
    expect(preview).not.toHaveTextContent(/Estimate/);
    expect(mock.bodies["post /api/v1/billing/subscriptions/{id}/change/preview"]!.at(-1)).toEqual({ price: "team_month", proration: "create_prorations", at_period_end: false });
    await userEvent.click(within(group).getByRole("checkbox", { name: "At period end" }));
    expect(await within(group).findByText(/nothing is charged now/)).toBeInTheDocument();
    await userEvent.click(within(group).getByRole("checkbox", { name: "At period end" }));
    await within(group).findByText(/net \$35\.00/);
    await userEvent.click(within(group).getByRole("button", { name: "Apply plan change" }));
    await waitFor(() => expect(st.subscriptions.find((s) => s.id === "sub1")!.items[0]!.price).toBe("team_month"));
    expect(lastBody(mock, "post /api/v1/billing/subscriptions/{id}/change")).toEqual({ price: "team_month", proration: "create_prorations", at_period_end: false });
  });

  it("starts a trial subscription", async () => {
    const { mock } = setup();
    renderStudio("/billing/subscriptions");
    await userEvent.click(await screen.findByRole("button", { name: "New subscription" }));
    const form = await sheet("New subscription");
    await userEvent.type(within(form).getByLabelText(/^Subscriber/), "usr_new");
    await pick(within(form).getByLabelText("Recurring price"), "pro_month");
    await userEvent.type(within(form).getByLabelText(/^Trial days/), "30");
    await userEvent.click(within(form).getByRole("button", { name: "Start subscription" }));
    await toast(/Subscription for usr_new created/);
    expect(lastBody(mock, "post /api/v1/billing/subscriptions")).toMatchObject({ subject: "usr_new", items: [{ price: "pro_month", quantity: 1 }], trial_days: 30 });
    expect(await within(screen.getByRole("table", { name: "Subscriptions" })).findByText("trialing")).toBeInTheDocument();
  });
});

describe("Orders", () => {
  it("lists all orders with paging and filters, and creates a one-time order", async () => {
    const { mock, st } = setup();
    st.orders = Array.from({ length: 60 }, (_, i) => ({ ...st.orders[0]!, id: `ord${i + 1}`, subject: i < 59 ? "org:acme" : "usr_ana", status: i === 0 ? "paid" : "open", invoice_id: i === 0 ? "inv3" : null }));
    renderStudio("/billing/orders");
    const table = await screen.findByRole("table", { name: "Orders" });
    expect(rows("Orders")).toHaveLength(51); // header + first page
    expect(await within(table).findByText("$10.00")).toBeInTheDocument();
    const paging = screen.getByLabelText("Orders paging");
    expect(paging).toHaveTextContent("1–50 of 60");
    expect(mock.bodies["get /api/v1/billing/admin/orders"]).toBeDefined();
    await userEvent.click(within(paging).getByRole("button", { name: "Next" }));
    await waitFor(() => expect(screen.getByLabelText("Orders paging")).toHaveTextContent("51–60 of 60"));
    expect(rows("Orders")).toHaveLength(11);
    await userEvent.type(screen.getByLabelText("Subject"), "usr_ana");
    await userEvent.click(screen.getByRole("button", { name: "Show orders" }));
    await waitFor(() => expect(rows("Orders")).toHaveLength(2));
    await userEvent.click(screen.getByRole("button", { name: "New order" }));
    const form = await sheet("New order");
    expect(within(form).getByLabelText("Customer")).toHaveValue("usr_ana");
    await pick(within(form).getByLabelText("One-time price"), "credits_once");
    await userEvent.click(within(form).getByRole("button", { name: "Create order" }));
    await toast("Order created");
    expect(lastBody(mock, "post /api/v1/billing/orders")).toMatchObject({ subject: "usr_ana", items: [{ price: "credits_once", quantity: 1 }] });
  });
});

describe("Usage meters", () => {
  it("lists instance-wide meters (750 / 1,000) with 70, 90 and 100 percent states", async () => {
    const { mock } = setup();
    renderStudio("/billing/usage");
    const meters = await screen.findByRole("list", { name: "Usage meters" });
    expect(within(meters).getByLabelText("api_calls usage")).toHaveTextContent("750 / 1,000");
    const state = (feature: string) => within(meters).getByLabelText(`${feature} usage`).closest("li")!.getAttribute("data-state");
    expect(state("api_calls")).toBe("warn70");
    expect(state("exports")).toBe("warn90");
    expect(state("seats")).toBe("full");
    expect(state("storage")).toBe("ok");
    expect(within(meters).getByRole("progressbar", { name: "api_calls quota" })).toHaveAttribute("aria-valuenow", "75");
    expect(meters).toHaveTextContent("org:acme");
    expect(mock.calls).not.toContain("get /api/v1/usage");
    await userEvent.type(screen.getByLabelText("Min % used"), "90");
    await userEvent.click(screen.getByRole("button", { name: "Show usage" }));
    await waitFor(() => expect(within(screen.getByRole("list", { name: "Usage meters" })).getAllByRole("listitem")).toHaveLength(2));
  });

  it("shows quota alerts for a filtered subject", async () => {
    setup();
    renderStudio("/billing/usage");
    await userEvent.type(await screen.findByLabelText("Subject"), "org:acme");
    await userEvent.click(screen.getByRole("button", { name: "Show usage" }));
    expect(await screen.findByRole("table", { name: "Quota alerts" })).toHaveTextContent("90%");
  });
});

describe("Invoices", () => {
  it("lists invoices, filters by status and opens frozen lines", async () => {
    setup();
    renderStudio("/billing/invoices");
    await screen.findByRole("table", { name: "Invoices" });
    expect(rows("Invoices")).toHaveLength(4);
    await pick(screen.getByLabelText("Status"), "open");
    await waitFor(() => expect(rows("Invoices")).toHaveLength(2));
    expect(within(screen.getByRole("table", { name: "Credit notes" })).getByText("CN-1")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("link", { name: "INV-inv2" }));
    const lines = await screen.findByRole("table", { name: "Invoice lines" });
    expect(lines).toHaveTextContent("Pro plan (monthly)");
    expect(screen.getByLabelText("Invoice totals")).toHaveTextContent("Amount due¥2,900");
  });

  it("shows tax per line and totals for a paid invoice", async () => {
    setup();
    renderStudio("/billing/invoices/inv1");
    const lines = await screen.findByRole("table", { name: "Invoice lines" });
    expect(lines).toHaveTextContent("$5.22");
    expect(lines).toHaveTextContent("18%");
    expect(screen.getByLabelText("Invoice totals")).toHaveTextContent("Total$34.22");
  });

  it("marks an open invoice paid and voids another after confirming", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/invoices/inv2");
    await userEvent.click(await screen.findByRole("button", { name: "Mark paid (out of band)" }));
    await waitFor(() => expect(st.invoices.find((i) => i.id === "inv2")!.status).toBe("paid"));
    expect(mock.calls).toContain("post /api/v1/billing/invoices/inv2/mark-paid");
    expect(await screen.findByRole("form", { name: "Credit note" })).toBeInTheDocument();
  });

  it("voids an open invoice", async () => {
    const { st } = setup();
    renderStudio("/billing/invoices/inv2");
    await userEvent.click(await screen.findByRole("button", { name: "Void invoice" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm void invoice" }));
    await waitFor(() => expect(st.invoices.find((i) => i.id === "inv2")!.status).toBe("void"));
  });

  it("refunds part of a paid invoice with a credit note in minor units, or credits the balance", async () => {
    const { mock } = setup();
    renderStudio("/billing/invoices/inv1");
    const form = await screen.findByRole("form", { name: "Credit note" });
    const amount = within(form).getByLabelText("Credit amount (USD)");
    expect(amount).toHaveValue("34.22");
    await userEvent.clear(amount);
    await userEvent.type(amount, "10.50");
    await userEvent.type(within(form).getByLabelText("Reason"), "partial");
    await userEvent.click(within(form).getByRole("button", { name: "Issue credit note" }));
    expect(await within(form).findByRole("status")).toHaveTextContent("Refund issued");
    expect(lastBody(mock, "post /api/v1/billing/invoices/{id}/credit-notes")).toMatchObject({ amount: 1050, to_balance: false, reason: "partial" });
    expect(await screen.findByRole("table", { name: "Invoice credit notes" })).toHaveTextContent("$10.50");
    await userEvent.click(within(form).getByRole("checkbox", { name: /Credit to customer balance/ }));
    await userEvent.click(within(form).getByRole("button", { name: "Issue credit note" }));
    await waitFor(() => expect(lastBody(mock, "post /api/v1/billing/invoices/{id}/credit-notes")).toMatchObject({ to_balance: true }));
  });
});

describe("Payments", () => {
  it("shows failed payments with their failure and the refunds", async () => {
    setup();
    renderStudio("/billing/payments");
    const table = await screen.findByRole("table", { name: "Payments" });
    expect(table).toHaveTextContent("card_declined: Your card was declined");
    expect(screen.getByLabelText("Payment summary")).toHaveTextContent("Failed1");
    await pick(screen.getByLabelText("Status"), "succeeded");
    expect(rows("Payments")).toHaveLength(2);
    expect(await within(await screen.findByRole("table", { name: "Refunds" })).findByText("$5.00")).toBeInTheDocument();
  });
});

describe("Coupons", () => {
  it("archives a coupon", async () => {
    const { st } = setup();
    renderStudio("/billing/coupons");
    await screen.findByRole("table", { name: "Coupons" });
    await userEvent.click(screen.getByRole("button", { name: "Archive coupon WELCOME15" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm archive coupon welcome15" }));
    await waitFor(() => expect(st.coupons[0]!.active).toBe(false));
    expect(await screen.findByRole("status")).toHaveTextContent("Coupon archived");
  });

  it("lists coupons and creates a percent coupon in basis points", async () => {
    const { mock } = setup();
    renderStudio("/billing/coupons");
    expect(await screen.findByRole("table", { name: "Coupons" })).toHaveTextContent("15% off");
    await userEvent.click(screen.getByRole("button", { name: "New coupon" }));
    const form = await sheet("New coupon");
    await userEvent.type(within(form).getByLabelText("Coupon code"), "SPRING");
    await userEvent.type(within(form).getByLabelText("Percent off"), "12.5");
    await pick(within(form).getByLabelText("Duration"), "repeating");
    await userEvent.type(within(form).getByLabelText("Cycles"), "3");
    await userEvent.click(within(form).getByRole("button", { name: "Save coupon" }));
    await toast(/Coupon SPRING created/);
    expect(lastBody(mock, "put /api/v1/billing/coupons/{code}")).toMatchObject({ kind: "percent", percent_bp: 1250, duration: "repeating", duration_cycles: 3 });
    expect(await screen.findByText("12.5% off")).toBeInTheDocument();
  });

  it("creates an amount coupon in minor units", async () => {
    const { mock } = setup();
    renderStudio("/billing/coupons");
    await userEvent.click(await screen.findByRole("button", { name: "New coupon" }));
    const form = await sheet("New coupon");
    await userEvent.type(within(form).getByLabelText("Coupon code"), "TENOFF");
    await pick(within(form).getByLabelText("Coupon kind"), "amount");
    await userEvent.type(within(form).getByLabelText("Amount off"), "10");
    await userEvent.click(within(form).getByRole("button", { name: "Save coupon" }));
    await waitFor(() => expect(lastBody(mock, "put /api/v1/billing/coupons/{code}")).toMatchObject({ kind: "amount", amount_off: 1000, currency: "USD" }));
  });
});

describe("Dunning", () => {
  it("lists invoices waiting on a retry, retries one and shows past-due subscriptions", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/dunning");
    const queue = await screen.findByRole("table", { name: "Dunning queue" });
    expect(queue).toHaveTextContent("INV-inv2");
    expect(queue).toHaveTextContent("card_declined");
    expect(await screen.findByLabelText("Retry schedule")).toHaveTextContent("Retries after 1d, 3d, 5d; then cancel the subscription");
    expect(await screen.findByRole("list", { name: "Past-due subscriptions" })).toHaveTextContent("usr_ana");
    await userEvent.click(screen.getByRole("button", { name: "Retry INV-inv2" }));
    await waitFor(() => expect(st.invoices.find((i) => i.id === "inv2")!.attempt_count).toBe(3));
    expect(mock.calls).toContain("post /api/v1/billing/invoices/inv2/pay");
    await userEvent.click(screen.getByRole("button", { name: "Run dunning now" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Maintenance ran");
  });
});

describe("Credits", () => {
  it("shows platform statistics, consumption and a subject ledger", async () => {
    const { mock } = setup();
    renderStudio("/billing/credits");
    const summary = await screen.findByLabelText("Credit summary");
    expect(summary).toHaveTextContent("Top-ups5,000");
    expect(summary).toHaveTextContent("Shortfall25");
    expect(await screen.findByRole("table", { name: "Credit consumption" })).toHaveTextContent("ai_generation");
    expect(screen.getByRole("list", { name: "Credit rates" })).toHaveTextContent("10 credits per 1 units");
    await userEvent.type(screen.getByLabelText("Subject"), "org:acme");
    await userEvent.click(screen.getByRole("button", { name: "Open ledger" }));
    expect(await screen.findByLabelText("Credit balance")).toHaveTextContent("960");
    expect(await screen.findByText("ledger consistent")).toBeInTheDocument();
    expect(rows("Credit ledger")).toHaveLength(3);
    await pick(screen.getByLabelText("Entry kind"), "top_up");
    await waitFor(() => expect(rows("Credit ledger")).toHaveLength(2));
    await userEvent.type(screen.getByLabelText("Credits"), "250");
    await userEvent.click(screen.getByRole("button", { name: "Add purchased credits" }));
    expect(await screen.findByLabelText("Credit balance")).toHaveTextContent("1,210");
    expect(lastBody(mock, "post /api/v1/credits/top-ups")).toMatchObject({ subject: "org:acme", amount: 250 });
  });
});

describe("Providers and settings", () => {
  it("keeps provider secrets write-only", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/settings");
    const razor = await screen.findByRole("region", { name: "razorpay provider" });
    expect(within(razor).getByText("Not configured.")).toBeInTheDocument();
    const stripe = screen.getByRole("region", { name: "stripe provider" });
    expect(stripe).toHaveTextContent("Key …c4f2");
    expect(within(stripe).getByLabelText("stripe secret key (write-only)")).toHaveValue("");
    await userEvent.type(within(razor).getByLabelText("razorpay secret key (write-only)"), "rzp_test_SECRET1234");
    await userEvent.type(within(razor).getByLabelText("razorpay webhook secret (write-only)"), "whsec_abc");
    await userEvent.type(within(razor).getByLabelText("razorpay key id"), "rzp_test_id");
    await userEvent.click(within(razor).getByRole("button", { name: "Save razorpay" }));
    expect(await within(razor).findByRole("status")).toHaveTextContent("Saved");
    expect(st.secrets.razorpay).toMatchObject({ api_key: "rzp_test_SECRET1234", webhook_secret: "whsec_abc" });
    // After saving the form is empty again and the page only knows the hint.
    expect(within(razor).getByLabelText("razorpay secret key (write-only)")).toHaveValue("");
    expect(razor).toHaveTextContent("Key …1234");
    expect(document.body.textContent).not.toContain("rzp_test_SECRET1234");
    expect(lastBody(mock, "put /api/v1/billing/providers/{p}")).toMatchObject({ enabled: true });
  });

  it("removes a provider after confirming", async () => {
    const { st } = setup();
    renderStudio("/billing/settings");
    const stripe = await screen.findByRole("region", { name: "stripe provider" });
    await userEvent.click(within(stripe).getByRole("button", { name: "Remove stripe" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm remove stripe" }));
    await waitFor(() => expect(st.providers).toHaveLength(0));
  });

  it("archives a tax rate", async () => {
    const { st } = setup();
    renderStudio("/billing/settings");
    const list = await screen.findByRole("list", { name: "Tax rates" });
    await userEvent.click(within(list).getByRole("button", { name: "Archive tax rate IN" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm archive tax rate in" }));
    await waitFor(() => expect(st.taxRates[0]!.active).toBe(false));
  });

  it("edits the dunning settings, tax rates and shows webhook events", async () => {
    const { mock } = setup();
    renderStudio("/billing/settings");
    const form = await screen.findByRole("form", { name: "Billing settings" });
    const retry = within(form).getByLabelText("Retry days");
    await userEvent.clear(retry);
    await userEvent.type(retry, "2, 4");
    await pick(within(form).getByLabelText("After the last retry"), "leave_past_due");
    await userEvent.click(within(form).getByRole("button", { name: "Save settings" }));
    await waitFor(() => expect(lastBody(mock, "put /api/v1/billing/settings")).toMatchObject({ retry_days: [2, 4], final_action: "leave_past_due" }));
    expect(screen.getByRole("list", { name: "Tax rates" })).toHaveTextContent("GST 18%");
    const tax = screen.getByRole("form", { name: "New tax rate" });
    await userEvent.type(within(tax).getByLabelText("Tax region"), "DE");
    await userEvent.type(within(tax).getByLabelText("Tax name"), "VAT");
    await userEvent.type(within(tax).getByLabelText("Rate percent"), "19");
    await userEvent.click(within(tax).getByRole("button", { name: "Add tax rate" }));
    await waitFor(() => expect(lastBody(mock, "put /api/v1/billing/tax-rates/{region}")).toEqual({ name: "VAT", rate_bp: 1900, active: true }));
    expect(await screen.findByRole("table", { name: "Webhook events" })).toHaveTextContent("invoice.paid");
  });
});

describe("Stores", () => {
  it("lists store products and purchases with status and subject", async () => {
    setup();
    renderStudio("/billing/stores");
    const products = await screen.findByRole("table", { name: "Store products" });
    expect(products).toHaveTextContent("com.acme.pro.monthly");
    expect(within(products).getByText("subscription")).toBeInTheDocument();
    const purchases = await screen.findByRole("table", { name: "Store purchases" });
    expect(within(purchases).getAllByRole("row")).toHaveLength(4);
    expect(purchases).toHaveTextContent("orig-1001");
    expect(within(purchases).getByRole("link", { name: "org:acme" })).toHaveAttribute("href", "/billing/customers?subject=org%3Aacme");
    expect(within(purchases).getByText("grace")).toBeInTheDocument();
    expect(within(purchases).getByText("auto-renews")).toBeInTheDocument();
  });

  it("filters purchases by status and by subject through the API", async () => {
    const { mock } = setup();
    renderStudio("/billing/stores");
    const purchases = await screen.findByRole("table", { name: "Store purchases" });
    await pick(screen.getByLabelText("Status"), "refunded");
    expect(within(purchases).getAllByRole("row")).toHaveLength(2);
    await pick(screen.getByLabelText("Status"), "");
    await userEvent.type(screen.getByLabelText("Subject"), "org:acme");
    await waitFor(() => expect(mock.calls.some((c) => c.includes("store-purchases"))).toBe(true));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Store purchases" })).getAllByRole("row")).toHaveLength(2));
  });

  it("filters store products by store", async () => {
    setup();
    renderStudio("/billing/stores");
    const products = await screen.findByRole("table", { name: "Store products" });
    await pick(screen.getByLabelText("Filter store"), "google_play");
    expect(products).not.toHaveTextContent("com.acme.pro.monthly");
    expect(products).toHaveTextContent("coins_100");
  });

  it("maps a store product to a price, re-maps it and deletes it", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/stores");
    await screen.findByRole("table", { name: "Store products" });
    await userEvent.click(screen.getByRole("button", { name: "Map a store product" }));
    const form = await sheet("Map a store product");
    await pick(within(form).getByLabelText("Mapping store"), "creem");
    await userEvent.type(within(form).getByLabelText("Store product id"), "prod_abc");
    expect(within(form).queryByRole("option", { name: /pro_tiers/ })).not.toBeInTheDocument(); // metered prices cannot be sold by a store
    await pick(within(form).getByLabelText("Sold as price"), "pro_month");
    await userEvent.click(within(form).getByRole("button", { name: "Save mapping" }));
    await toast("Mapping saved");
    expect(lastBody(mock, "put /api/v1/billing/store-products/{provider}/{product_id}")).toEqual({ price: "pro_month" });
    expect(mock.calls).toContain("put /api/v1/billing/store-products/creem/prod_abc");
    expect(st.storeProducts.find((p) => p.product_id === "prod_abc")).toMatchObject({ provider: "creem", kind: "subscription" });
    // Edit loads the row into the form; saving re-maps it.
    await waitFor(() => expect(screen.queryByRole("form", { name: "Map a store product" })).not.toBeInTheDocument());
    await userEvent.click(await screen.findByRole("button", { name: "Edit app_store com.acme.pro.monthly" }));
    const edit = await sheet("Edit mapping");
    expect(within(edit).getByLabelText("Store product id")).toHaveValue("com.acme.pro.monthly");
    await pick(within(edit).getByLabelText("Sold as price"), "pro_month");
    await userEvent.click(within(edit).getByRole("button", { name: "Save mapping" }));
    await waitFor(() => expect(st.storeProducts.find((p) => p.product_id === "com.acme.pro.monthly")!.price).toBe("pro_month"));
    // Delete needs a confirmation.
    await waitFor(() => expect(screen.queryByRole("form", { name: "Edit mapping" })).not.toBeInTheDocument());
    await userEvent.click(screen.getByRole("button", { name: "Delete google_play coins_100" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete google_play coins_100" }));
    await waitFor(() => expect(st.storeProducts.some((p) => p.product_id === "coins_100")).toBe(false));
    expect(mock.calls).toContain("delete /api/v1/billing/store-products/google_play/coins_100");
  });

  it("shows empty states", async () => {
    const { st } = setup();
    st.storeProducts = [];
    st.storePurchases = [];
    renderStudio("/billing/stores");
    expect(await screen.findByText(/No store products mapped/)).toBeInTheDocument();
    expect(await screen.findByText("No store purchases match")).toBeInTheDocument();
  });

  it("is a tab of the studio", async () => {
    setup();
    renderStudio("/billing");
    await userEvent.click(within(await screen.findByRole("tablist", { name: "Billing groups" })).getByRole("tab", { name: "Catalog" }));
    const nav = await screen.findByRole("tablist", { name: "Catalog pages" });
    expect(within(nav).getByRole("tab", { name: "Stores" })).toHaveAttribute("href", "/billing/stores");
  });
});

describe("Store and regional provider forms", () => {
  it("shows test mode for each provider and keeps every secret write-only (app_store)", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/settings");
    const app = await screen.findByRole("region", { name: "app_store provider" });
    expect(within(app).getByLabelText("app_store test mode")).toHaveTextContent(/Test mode.*Sandbox only/);
    for (const id of ["google_play", "creem", "wechat_pay"]) expect(screen.getByLabelText(`${id} test mode`)).toBeInTheDocument();
    expect(within(app).getByText("not configured")).toBeInTheDocument();
    await userEvent.type(within(app).getByLabelText("app_store Issuer id"), "issuer-1");
    await userEvent.type(within(app).getByLabelText("app_store App Store Connect key id"), "KEY123");
    await userEvent.type(within(app).getByLabelText("app_store Bundle id"), "com.acme.app");
    await pick(within(app).getByLabelText("app_store Environment"), "xcode");
    await userEvent.click(within(app).getByLabelText(/^app_store App Store Connect private key/));
    await userEvent.paste("-----BEGIN PRIVATE KEY-----SECRETPEM");
    await userEvent.click(within(app).getByRole("button", { name: "Save app_store" }));
    expect(await within(app).findByRole("status")).toHaveTextContent("Saved");
    expect(st.secrets.app_store!.config).toEqual({ issuer_id: "issuer-1", key_id: "KEY123", bundle_id: "com.acme.app", environment: "xcode", private_key: "-----BEGIN PRIVATE KEY-----SECRETPEM" });
    expect(lastBody(mock, "put /api/v1/billing/providers/{p}")).toMatchObject({ enabled: true });
    expect(within(app).getByLabelText(/^app_store App Store Connect private key/)).toHaveValue("");
    expect(within(app).getByText("configured")).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("SECRETPEM");
  });

  it("reads test mode and config back and keeps a blank secret (app_store)", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/settings");
    const app = await screen.findByRole("region", { name: "app_store provider" });
    await userEvent.type(within(app).getByLabelText("app_store Issuer id"), "issuer-1");
    await userEvent.type(within(app).getByLabelText("app_store App Store Connect key id"), "KEY123");
    await userEvent.type(within(app).getByLabelText("app_store Bundle id"), "com.acme.app");
    await pick(within(app).getByLabelText("app_store Environment"), "xcode");
    await userEvent.click(within(app).getByLabelText(/^app_store App Store Connect private key/));
    await userEvent.paste("-----BEGIN PRIVATE KEY-----SECRETPEM");
    await userEvent.click(within(app).getByRole("button", { name: "Save app_store" }));
    expect(await within(app).findByRole("status")).toHaveTextContent("Saved");
    // Test mode and the non-secret config come back from the server; the secret does not.
    expect(within(app).getByLabelText("app_store test mode")).toHaveTextContent("Test mode (xcode)");
    expect(within(app).getByLabelText("app_store Bundle id")).toHaveValue("com.acme.app");
    expect(within(app).getByLabelText("app_store Environment")).toHaveTextContent("xcode");
    const pk = within(app).getByLabelText(/^app_store App Store Connect private key/);
    expect(pk).toHaveValue("");
    expect(pk).toHaveAttribute("placeholder", "leave blank to keep");
    expect(pk).not.toBeRequired();
    // Change one field, leave the secret blank: it is not sent and the server keeps it.
    await userEvent.clear(within(app).getByLabelText("app_store Bundle id"));
    await userEvent.type(within(app).getByLabelText("app_store Bundle id"), "com.acme.other");
    await userEvent.click(within(app).getByRole("button", { name: "Save app_store" }));
    await waitFor(() => expect(st.secrets.app_store!.config).toMatchObject({ bundle_id: "com.acme.other" }));
    expect(JSON.stringify(lastBody(mock, "put /api/v1/billing/providers/{p}"))).not.toContain("private_key");
    expect(st.secrets.app_store!.config).toMatchObject({ private_key: "-----BEGIN PRIVATE KEY-----SECRETPEM", environment: "xcode" });
    expect(document.body.textContent).not.toContain("SECRETPEM");
  });

  it("saves google_play config", async () => {
    const { st } = setup();
    renderStudio("/billing/settings");
    const gp = await screen.findByRole("region", { name: "google_play provider" });
    await userEvent.type(within(gp).getByLabelText("google_play Package name"), "com.acme.app");
    await userEvent.type(within(gp).getByLabelText("google_play Push audience"), "https://api.example/push");
    await userEvent.type(within(gp).getByLabelText("google_play Push service account email"), "push@acme.iam.gserviceaccount.com");
    await userEvent.click(within(gp).getByLabelText(/^google_play Service account JSON/));
    await userEvent.paste('{"type":"service_account"}');
    await userEvent.click(within(gp).getByRole("button", { name: "Save google_play" }));
    expect(await within(gp).findByRole("status")).toHaveTextContent("Saved");
    expect(st.secrets.google_play!.config).toMatchObject({ package_name: "com.acme.app", service_account: '{"type":"service_account"}' });
  });

  it("saves creem keys with the key hint only", async () => {
    const { st } = setup();
    renderStudio("/billing/settings");
    const creem = await screen.findByRole("region", { name: "creem provider" });
    await userEvent.type(within(creem).getByLabelText("creem Creem API key (write-only)"), "creem_test_ABCDEFGH1234");
    await userEvent.type(within(creem).getByLabelText("creem Creem webhook secret (write-only)"), "whsec_creem_1");
    await userEvent.type(within(creem).getByLabelText("creem API base (optional)"), "https://test-api.creem.io");
    await userEvent.click(within(creem).getByRole("button", { name: "Save creem" }));
    expect(await within(creem).findByRole("status")).toHaveTextContent("Saved");
    expect(st.secrets.creem).toMatchObject({ api_key: "creem_test_ABCDEFGH1234", webhook_secret: "whsec_creem_1" });
    expect(creem).toHaveTextContent("Key …1234");
    expect(creem).toHaveTextContent("https://test-api.creem.io");
    expect(document.body.textContent).not.toContain("creem_test_ABCDEFGH1234");
  });

  it("wechat_pay requires an API base and valid platform keys JSON, then saves", async () => {
    const { mock, st } = setup();
    renderStudio("/billing/settings");
    const wx = await screen.findByRole("region", { name: "wechat_pay provider" });
    await userEvent.type(within(wx).getByLabelText("wechat_pay Merchant id (mchid)"), "1900000001");
    await userEvent.type(within(wx).getByLabelText(/^wechat_pay APIv3 key/), "0123456789abcdef0123456789abcdef");
    await userEvent.type(within(wx).getByLabelText("wechat_pay App id"), "wx123");
    await userEvent.type(within(wx).getByLabelText("wechat_pay Merchant certificate serial"), "ABCD");
    await userEvent.type(within(wx).getByLabelText("wechat_pay Notify URL"), "https://app/notify");
    await userEvent.click(within(wx).getByLabelText(/^wechat_pay Merchant private key/));
    await userEvent.paste("PEMKEY");
    await userEvent.click(within(wx).getByLabelText(/^wechat_pay Platform keys/));
    await userEvent.paste("not json");
    await userEvent.click(within(wx).getByRole("button", { name: "Save wechat_pay" }));
    expect(await within(wx).findByRole("alert")).toHaveTextContent("platform_keys must be valid JSON");
    await userEvent.clear(within(wx).getByLabelText(/^wechat_pay Platform keys/));
    await userEvent.click(within(wx).getByLabelText(/^wechat_pay Platform keys/));
    await userEvent.paste('[{"serial":"S1","pem":"PEM"}]');
    await userEvent.click(within(wx).getByRole("button", { name: "Save wechat_pay" }));
    expect(await within(wx).findByRole("alert")).toHaveTextContent("API base is required");
    expect(mock.calls).not.toContain("put /api/v1/billing/providers/wechat_pay");
    await userEvent.type(within(wx).getByLabelText("wechat_pay API base"), "https://mock.wechat.test");
    await userEvent.click(within(wx).getByRole("button", { name: "Save wechat_pay" }));
    expect(await within(wx).findByRole("status")).toHaveTextContent("Saved");
    expect(st.secrets.wechat_pay!.config).toMatchObject({ appid: "wx123", platform_keys: [{ serial: "S1", pem: "PEM" }] });
    expect(lastBody(mock, "put /api/v1/billing/providers/{p}")).toMatchObject({ key_id: "1900000001", api_base: "https://mock.wechat.test", enabled: true });
  });

  it("removes a store provider after confirming", async () => {
    const { st } = setup();
    st.providers.push({ provider: "creem", key_id: null, hint: "…1234", api_base: null, enabled: true, webhook_path: "/api/v1/billing/webhooks/creem", updated_at: "2026-11-01T00:00:00Z", test_mode: true, environment: "test", config: {}, secrets_set: ["api_key"] });
    renderStudio("/billing/settings");
    const creem = await screen.findByRole("region", { name: "creem provider" });
    await userEvent.click(within(creem).getByRole("button", { name: "Remove creem" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm remove creem" }));
    await waitFor(() => expect(st.providers.some((p) => p.provider === "creem")).toBe(false));
  });
});
