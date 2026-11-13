import { describe, expect, it } from "vitest";
import { ConflictError, ForgeBase, NotFoundError, PermissionError, ValidationError } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, b: new ForgeBase("http://x", { fetch: f.fetch }).billing };
};
const body = (c: { body?: string }) => JSON.parse(c.body!) as unknown;

describe("billing.customers", () => {
  it("get passes the subject, put maps camelCase to the wire", async () => {
    const { f, b } = setup(json(200, { id: "cus_1", subject: "org:1" }), json(200, { id: "cus_1" }));
    expect(await b.customers.get("org:1")).toMatchObject({ id: "cus_1" });
    await b.customers.put({ email: "a@b.c", taxExempt: true, region: "DE" });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/billing/customer?subject=org%3A1" });
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/billing/customer" });
    expect(body(f.calls[1]!)).toEqual({ email: "a@b.c", tax_exempt: true, region: "DE" });
  });

  it("list unwraps items and sends the limit", async () => {
    const { f, b } = setup(json(200, { items: [{ id: "cus_1" }] }));
    expect(await b.customers.list(5)).toEqual([{ id: "cus_1" }]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/customers?limit=5");
  });

  it("a missing customer is a NotFoundError", async () => {
    const { b } = setup(json(404, errBody("not_found")));
    await expect(b.customers.get()).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("billing.portal", () => {
  it("get reads the portal of a subject", async () => {
    const { f, b } = setup(json(200, { invoices: [], payments: [], credit_notes: [], subscriptions: [] }));
    expect((await b.portal.get("org:2")).invoices).toEqual([]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/portal?subject=org%3A2");
  });

  it("paymentMethod posts the return url and sends {} by default", async () => {
    const { f, b } = setup(json(200, { session_id: "s", url: "https://pay/x" }));
    expect(await b.portal.paymentMethod({ returnUrl: "https://app/back" })).toEqual({ session_id: "s", url: "https://pay/x" });
    await b.portal.paymentMethod();
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/portal/payment-method" });
    expect(body(f.calls[0]!)).toEqual({ return_url: "https://app/back" });
    expect(body(f.calls[1]!)).toEqual({});
  });
});

describe("billing.subscriptions", () => {
  const sub = { id: "sub_1", status: "active" };

  it("list and get", async () => {
    const { f, b } = setup(json(200, { items: [sub] }), json(200, sub));
    expect(await b.subscriptions.list()).toEqual([sub]);
    expect(await b.subscriptions.get("sub/1")).toEqual(sub);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/subscriptions");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/subscriptions/sub%2F1");
  });

  it("create sends items, the idempotency key and the trial", async () => {
    const { f, b } = setup(json(201, { replayed: false, subscription: sub, payment_url: "https://pay/1" }));
    const out = await b.subscriptions.create({ idempotencyKey: "k1", items: [{ price: "pro_month", quantity: 2 }], trialDays: 7, returnUrl: "https://app/ok", coupon: "LAUNCH" });
    expect(out.payment_url).toBe("https://pay/1");
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/subscriptions" });
    expect(body(f.calls[0]!)).toEqual({
      idempotency_key: "k1",
      items: [{ price: "pro_month", quantity: 2 }],
      trial_days: 7,
      return_url: "https://app/ok",
      coupon: "LAUNCH",
    });
  });

  it("a second live subscription is a ConflictError", async () => {
    const { b } = setup(json(409, errBody("already_subscribed")));
    await expect(b.subscriptions.create({ idempotencyKey: "k", items: [{ price: "p" }] })).rejects.toBeInstanceOf(ConflictError);
  });

  it("change and previewChange share the body", async () => {
    const { f, b } = setup(json(200, { replayed: false }), json(200, { net: 120, lines: [] }));
    await b.subscriptions.change("sub_1", { price: "team_month", atPeriodEnd: false, proration: "create_prorations", quantity: 3 });
    expect((await b.subscriptions.previewChange("sub_1", { price: "team_month" })).net).toBe(120);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/subscriptions/sub_1/change" });
    expect(body(f.calls[0]!)).toEqual({ price: "team_month", at_period_end: false, proration: "create_prorations", quantity: 3 });
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/subscriptions/sub_1/change/preview" });
    expect(body(f.calls[1]!)).toEqual({ price: "team_month" });
  });

  it("cancel defaults to an empty body, pause and resume send none", async () => {
    const { f, b } = setup(json(200, { replayed: false }), json(200, { ...sub, status: "paused" }), json(200, { replayed: false }));
    await b.subscriptions.cancel("sub_1");
    await b.subscriptions.cancel("sub_1", { atPeriodEnd: false, reason: "too dear" });
    await b.subscriptions.pause("sub_1");
    await b.subscriptions.resume("sub_1");
    expect(f.calls.map((c) => c.url)).toEqual([
      "http://x/api/v1/billing/subscriptions/sub_1/cancel",
      "http://x/api/v1/billing/subscriptions/sub_1/cancel",
      "http://x/api/v1/billing/subscriptions/sub_1/pause",
      "http://x/api/v1/billing/subscriptions/sub_1/resume",
    ]);
    expect(body(f.calls[0]!)).toEqual({});
    expect(body(f.calls[1]!)).toEqual({ at_period_end: false, reason: "too dear" });
    expect(f.calls[2]!.body).toBeUndefined();
    expect(f.calls.every((c) => c.method === "POST")).toBe(true);
  });
});

describe("billing.orders", () => {
  it("list, get, create", async () => {
    const order = { id: "ord_1", status: "paid" };
    const { f, b } = setup(json(200, { items: [order] }), json(200, order), json(201, { replayed: false, order }));
    expect(await b.orders.list("org:3")).toEqual([order]);
    expect(await b.orders.get("ord_1")).toEqual(order);
    expect((await b.orders.create({ idempotencyKey: "k", items: [{ price: "topup_100" }] })).order).toEqual(order);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/orders?subject=org%3A3");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/orders/ord_1");
    expect(f.calls[2]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/orders" });
    expect(body(f.calls[2]!)).toEqual({ idempotency_key: "k", items: [{ price: "topup_100" }] });
  });

  it("listAll pages through the admin view", async () => {
    const { f, b } = setup(json(200, { items: [], limit: 10, offset: 20, total: 0 }));
    expect((await b.orders.listAll({ subject: "org:1", status: "paid", limit: 10, offset: 20 })).offset).toBe(20);
    await b.orders.listAll();
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/admin/orders?subject=org%3A1&status=paid&limit=10&offset=20");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/admin/orders");
  });
});

describe("billing.invoices", () => {
  it("list filters by subject, status and limit", async () => {
    const { f, b } = setup(json(200, { items: [{ id: "in_1" }] }));
    expect(await b.invoices.list({ subject: "org:1", status: "open", limit: 20 })).toEqual([{ id: "in_1" }]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/invoices?subject=org%3A1&status=open&limit=20");
  });

  it("get, void and the operator actions hit their routes", async () => {
    const { f, b } = setup(json(200, { id: "in_1" }), json(200, { id: "in_1", status: "void" }), json(200, { id: "in_1", status: "paid" }));
    await b.invoices.get("in_1");
    await b.invoices.void("in_1");
    await b.invoices.markPaid("in_1", { reference: "wire-77" });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/billing/invoices/in_1" });
    expect(f.calls[1]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/invoices/in_1/void" });
    expect(f.calls[1]!.body).toBeUndefined();
    expect(f.calls[2]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/invoices/in_1/mark-paid" });
    expect(body(f.calls[2]!)).toEqual({ reference: "wire-77" });
  });

  it("pay returns the hosted page and sends {} without options", async () => {
    const { f, b } = setup(json(200, { invoice: { id: "in_1" }, payment_url: "https://pay/in_1" }));
    expect((await b.invoices.pay("in_1", { returnUrl: "https://app/ok" })).payment_url).toBe("https://pay/in_1");
    await b.invoices.pay("in_1");
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/invoices/in_1/pay" });
    expect(body(f.calls[0]!)).toEqual({ return_url: "https://app/ok" });
    expect(body(f.calls[1]!)).toEqual({});
  });

  it("creditNote maps the options and a missing permission is a PermissionError", async () => {
    const { f, b } = setup(json(201, { id: "cn_1" }), json(403, errBody("forbidden")));
    expect(await b.invoices.creditNote("in_1", { idempotencyKey: "k", amount: 500, toBalance: true, reason: "goodwill" })).toEqual({ id: "cn_1" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/invoices/in_1/credit-notes" });
    expect(body(f.calls[0]!)).toEqual({ idempotency_key: "k", amount: 500, to_balance: true, reason: "goodwill" });
    await expect(b.invoices.creditNote("in_1", { idempotencyKey: "k2" })).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("billing credit notes, payments, refunds", () => {
  it("each list reads its route with subject and limit", async () => {
    const { f, b } = setup(json(200, { items: [{ id: "a" }] }));
    expect(await b.creditNotes.list({ subject: "org:1", limit: 3 })).toEqual([{ id: "a" }]);
    await b.payments.list({ limit: 4 });
    await b.refunds.list();
    expect(f.calls.map((c) => c.url)).toEqual([
      "http://x/api/v1/billing/credit-notes?subject=org%3A1&limit=3",
      "http://x/api/v1/billing/payments?limit=4",
      "http://x/api/v1/billing/refunds",
    ]);
  });
});

describe("billing.catalog", () => {
  it("products: list, get, put, archive", async () => {
    const p = { key: "pro", active: true };
    const { f, b } = setup(json(200, { items: [p] }), json(200, p), json(200, p), json(200, { ...p, active: false }));
    expect(await b.catalog.products.list()).toEqual([p]);
    await b.catalog.products.get("pro");
    await b.catalog.products.put("pro", { name: "Pro", kind: "plan", planKey: "pro" });
    expect((await b.catalog.products.archive("pro")).active).toBe(false);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/billing/products",
      "GET http://x/api/v1/billing/products/pro",
      "PUT http://x/api/v1/billing/products/pro",
      "POST http://x/api/v1/billing/products/pro/archive",
    ]);
    expect(body(f.calls[2]!)).toEqual({ name: "Pro", kind: "plan", plan_key: "pro" });
  });

  it("prices: list by product, get, put with multi-currency options, archive", async () => {
    const { f, b } = setup(json(200, { items: [] }), json(200, { key: "pro_month" }), json(200, { key: "pro_month" }), json(200, { key: "pro_month", active: false }));
    await b.catalog.prices.list("pro");
    await b.catalog.prices.get("pro_month");
    await b.catalog.prices.put("pro_month", {
      product: "pro",
      type: "recurring",
      currency: "usd",
      unitAmount: 1900,
      interval: "month",
      intervalCount: 1,
      trialDays: 14,
      currencyOptions: { eur: { unit_amount: 1700 } },
    });
    await b.catalog.prices.archive("pro_month");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/billing/prices?product=pro",
      "GET http://x/api/v1/billing/prices/pro_month",
      "PUT http://x/api/v1/billing/prices/pro_month",
      "POST http://x/api/v1/billing/prices/pro_month/archive",
    ]);
    expect(body(f.calls[2]!)).toEqual({
      product: "pro",
      type: "recurring",
      currency: "usd",
      unit_amount: 1900,
      interval: "month",
      interval_count: 1,
      trial_days: 14,
      currency_options: { eur: { unit_amount: 1700 } },
    });
  });

  it("changing the amounts of a price in use is a ConflictError", async () => {
    const { b } = setup(json(409, errBody("price_in_use")));
    await expect(b.catalog.prices.put("pro_month", { product: "pro", type: "recurring", currency: "usd" })).rejects.toBeInstanceOf(ConflictError);
  });

  it("coupons: list, put, archive", async () => {
    const { f, b } = setup(json(200, { items: [] }), json(200, { code: "SAVE 10" }), json(200, { code: "SAVE 10", active: false }));
    await b.catalog.coupons.list();
    await b.catalog.coupons.put("SAVE 10", { kind: "percent", percentBp: 1000, maxRedemptions: 50, expiresAt: "2030-01-01T00:00:00Z" });
    await b.catalog.coupons.archive("SAVE 10");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/billing/coupons",
      "PUT http://x/api/v1/billing/coupons/SAVE%2010",
      "POST http://x/api/v1/billing/coupons/SAVE%2010/archive",
    ]);
    expect(body(f.calls[1]!)).toEqual({ kind: "percent", percent_bp: 1000, max_redemptions: 50, expires_at: "2030-01-01T00:00:00Z" });
  });

  it("taxRates: list, put, archive", async () => {
    const { f, b } = setup(json(200, { items: [] }), json(200, { region: "US-CA" }), json(200, { region: "US-CA", active: false }));
    await b.catalog.taxRates.list();
    await b.catalog.taxRates.put("US-CA", { name: "CA sales tax", rateBp: 725 });
    await b.catalog.taxRates.archive("US-CA");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/billing/tax-rates",
      "PUT http://x/api/v1/billing/tax-rates/US-CA",
      "POST http://x/api/v1/billing/tax-rates/US-CA/archive",
    ]);
    expect(body(f.calls[1]!)).toEqual({ name: "CA sales tax", rate_bp: 725 });
  });
});

describe("billing.providers and settings", () => {
  it("providers: list, put (live keys are refused), delete", async () => {
    const { f, b } = setup(json(200, { items: [{ provider: "stripe" }] }), json(200, { provider: "stripe" }), json(400, errBody("live_keys_refused")), json(204, null));
    expect(await b.providers.list()).toEqual([{ provider: "stripe" }]);
    await b.providers.put("stripe", { apiKey: "sk_test_x", webhookSecret: "whsec_x", enabled: true, clear: ["api_base"] });
    await expect(b.providers.put("stripe", { apiKey: "sk_live_x" })).rejects.toBeInstanceOf(ValidationError);
    await expect(b.providers.delete("stripe")).resolves.toBeUndefined();
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/billing/providers/stripe" });
    expect(body(f.calls[1]!)).toEqual({ api_key: "sk_test_x", webhook_secret: "whsec_x", enabled: true, clear: ["api_base"] });
    expect(f.calls[3]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/billing/providers/stripe" });
  });

  it("settings: get and put the whole object", async () => {
    const wireSettings = { credit_note_prefix: "CN-", default_currency: "usd", final_action: "cancel", incomplete_days: 3, invoice_prefix: "INV-", retry_days: [1, 3, 5], webhook_max_age_secs: 86400, webhook_tolerance_secs: 300 };
    const { f, b } = setup(json(200, wireSettings), json(200, wireSettings));
    const s = await b.settings.get();
    expect(s.retry_days).toEqual([1, 3, 5]);
    await b.settings.put({
      creditNotePrefix: "CN-",
      defaultCurrency: "usd",
      finalAction: "cancel",
      incompleteDays: 3,
      invoicePrefix: "INV-",
      retryDays: [1, 3, 5],
      webhookMaxAgeSecs: 86400,
      webhookToleranceSecs: 300,
    });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/billing/settings" });
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/billing/settings" });
    expect(body(f.calls[1]!)).toEqual(wireSettings);
  });
});

describe("billing operator views", () => {
  it("stats sends the window, Dates as RFC 3339", async () => {
    const { f, b } = setup(json(200, { recurring: [], revenue: [] }));
    await b.stats({ from: new Date("2026-01-01T00:00:00Z"), to: "2026-02-01T00:00:00Z", interval: "month" });
    await b.stats();
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/stats?from=2026-01-01T00%3A00%3A00.000Z&to=2026-02-01T00%3A00%3A00Z&interval=month");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/billing/stats");
  });

  it("usageMeters maps minPercent", async () => {
    const { f, b } = setup(json(200, { items: [], limit: 50, offset: 0, total: 0 }));
    await b.usageMeters({ subject: "org:1", feature: "api_calls", minPercent: 90, limit: 5, offset: 10 });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/admin/usage-meters?subject=org%3A1&feature=api_calls&min_percent=90&limit=5&offset=10");
  });

  it("dunning unwraps the queue", async () => {
    const { f, b } = setup(json(200, { items: [{ invoice_id: "in_1", retries_left: 2 }] }));
    expect(await b.dunning(10)).toEqual([{ invoice_id: "in_1", retries_left: 2 }]);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/billing/dunning?limit=10");
  });

  it("maintenance posts and returns the report", async () => {
    const { f, b } = setup(json(200, { renewed: 2, retries: 1 }), json(403, errBody("forbidden")));
    expect(await b.maintenance()).toEqual({ renewed: 2, retries: 1 });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/billing/maintenance" });
    await expect(b.maintenance()).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("billing.store admin", () => {
  it("products, putProduct and deleteProduct", async () => {
    const sp = { provider: "app_store", product_id: "com.x.pro", price: "pro_month", kind: "subscription", updated_at: "t" };
    const { f, b } = setup(json(200, { items: [sp] }), json(200, sp), json(204, null));
    expect(await b.store.products()).toEqual([sp]);
    await b.store.putProduct("app_store", "com.x.pro", "pro_month");
    await b.store.deleteProduct("app_store", "com.x.pro");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/billing/store-products",
      "PUT http://x/api/v1/billing/store-products/app_store/com.x.pro",
      "DELETE http://x/api/v1/billing/store-products/app_store/com.x.pro",
    ]);
    expect(body(f.calls[1]!)).toEqual({ price: "pro_month" });
  });
});
