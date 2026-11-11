/** End-to-end smoke of the Billing Studio: drives every screen through the real router against the in-memory billing API. */
import { GROUPS as BILLING_GROUPS } from "./pages/billing/groups";
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, signedIn } from "./test/helpers";
import { MockApi } from "./test/mockApi";
import { installBilling } from "./test/billingMock";

afterEach(() => vi.unstubAllGlobals());

it("walks every Billing Studio screen from the sidebar and runs one real action on the main flows", async () => {
  const mock = signedIn(new MockApi().install());
  const st = installBilling(mock);
  renderStudio("/");
  const u = userEvent.setup();

  await u.click(await screen.findByRole("link", { name: "Billing" }));
  expect(await screen.findByRole("region", { name: "Recurring revenue" })).toHaveTextContent("$1,299.00");
  // Four groups on top, the pages of the active group below (A-23).
  const go = async (name: string) => {
    const g = BILLING_GROUPS.find((x) => x.pages.some((p) => p.label === name))!;
    await u.click(within(screen.getByRole("tablist", { name: "Billing groups" })).getByRole("tab", { name: g.label }));
    if (g.pages.length > 1) await u.click(within(await screen.findByRole("tablist", { name: `${g.label} pages` })).getByRole("tab", { name }));
  };

  await go("Catalog");
  expect(await screen.findByRole("table", { name: "Products" })).toHaveTextContent("Pro");

  await go("Customers");
  expect(await screen.findByRole("table", { name: "Customers" })).toHaveTextContent("org:acme");

  await go("Subscriptions");
  await screen.findByRole("table", { name: "Subscriptions" });
  await u.click(screen.getByRole("button", { name: "Manage sub1" }));
  await u.click(await screen.findByRole("button", { name: "Pause" }));
  await waitFor(() => expect(st.subscriptions[0]!.status).toBe("paused"));

  await go("Orders");
  await u.type(await screen.findByLabelText("Order subject"), "org:acme");
  await u.click(screen.getByRole("button", { name: "Show orders" }));
  expect(await screen.findByRole("table", { name: "Orders" })).toHaveTextContent("ord1");

  await go("Usage meters");
  await u.type(await screen.findByLabelText("Usage subject"), "org:acme");
  await u.click(screen.getByRole("button", { name: "Show usage" }));
  expect(await screen.findByLabelText("api_calls usage")).toHaveTextContent("750 / 1,000");

  await go("Invoices");
  await u.click(await screen.findByRole("link", { name: "INV-inv1" }));
  expect(await screen.findByRole("table", { name: "Invoice lines" })).toHaveTextContent("Pro plan (monthly)");
  await u.click(screen.getByRole("link", { name: "Back to invoices" }));

  await go("Payments");
  expect(await screen.findByRole("table", { name: "Payments" })).toHaveTextContent("card_declined");

  await go("Coupons");
  expect(await screen.findByRole("table", { name: "Coupons" })).toHaveTextContent("WELCOME15");

  await go("Dunning");
  expect(await screen.findByRole("table", { name: "Dunning queue" })).toHaveTextContent("INV-inv2");

  await go("Credits");
  expect(await screen.findByLabelText("Credit summary")).toHaveTextContent("Top-ups5,000");

  await go("Providers and settings");
  expect(await screen.findByRole("region", { name: "stripe provider" })).toHaveTextContent("…c4f2");
  expect(await screen.findByRole("table", { name: "Webhook events" })).toHaveTextContent("invoice.paid");

  // Nothing outside the fake's routes was called.
  expect(mock.problems).toEqual([]);
  expect(mock.calls.some((c) => c.startsWith("get /api/v1/billing/stats"))).toBe(true);
});

it("shows the disabled state from the sidebar when billing is off", async () => {
  const mock = signedIn(new MockApi().install());
  installBilling(mock, { disabled: true });
  renderStudio("/billing/invoices");
  expect(await screen.findByRole("status", { name: "Billing disabled" })).toBeInTheDocument();
});
