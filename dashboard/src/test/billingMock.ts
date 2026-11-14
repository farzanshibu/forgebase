/**
 * In-memory fake of the billing, credits and entitlements REST APIs for component tests.
 * Every request and response goes through MockApi, which checks it against the OpenAPI fixture (billing enabled):
 * routes the fixture covers are contract-checked (`mock.problems` must stay empty); the credits and entitlements
 * routes the fixture does not describe are plain handlers.
 * `installBilling(mock, { disabled: true })` answers every billing route 404, like a server with billing off.
 */
import type {
  BillingSettings, CreditNote, Coupon, Customer, Invoice, LedgerEntry, Order, Payment, Price, Product, ProviderAccount, Refund,
  Subscription, UsageSummary, WebhookEvent, StoreProduct, StorePurchase, WebhookDeliveryDetail,
} from "../api";
import { envelope, type MockApi } from "./mockApi";

const T0 = "2026-11-01T00:00:00Z";
const T1 = "2026-12-01T00:00:00Z";

export interface BillingState {
  products: Product[];
  prices: Price[];
  customers: Customer[];
  subscriptions: Subscription[];
  orders: Order[];
  invoices: Invoice[];
  payments: Payment[];
  refunds: Refund[];
  notes: CreditNote[];
  coupons: Coupon[];
  providers: ProviderAccount[];
  /** Secrets as received: the API must never echo them back. */
  secrets: Record<string, { api_key?: string | null; webhook_secret?: string | null; config?: Record<string, unknown> | null }>;
  storeProducts: StoreProduct[];
  storePurchases: StorePurchase[];
  settings: BillingSettings;
  webhooks: WebhookEvent[];
  /** The webhook inspector (ADR-0032), plain handlers: the OpenAPI fixture predates it. */
  deliveries: WebhookDeliveryDetail[];
  /** What a replay and a re-verify answer: the `result` word. */
  replayAs: string;
  reverifyAs: string;
  calls: { replay: string[]; reverify: string[] };
  usage: UsageSummary[];
  ledger: LedgerEntry[];
  creditBalance: number;
  taxRates: { id: string; region: string; name: string; rate_bp: number; active: boolean }[];
}

export const product = (key: string, over: Partial<Product> = {}): Product => ({
  id: `prod_${key}`, key, name: key[0]!.toUpperCase() + key.slice(1), description: null, kind: "plan", plan_key: key, addon_key: null, pack_key: null,
  credits: null, active: true, metadata: {}, created_at: T0, updated_at: T0, ...over,
});

export const price = (key: string, productKey: string, over: Partial<Price> = {}): Price => ({
  id: `price_${key}`, key, product_id: `prod_${productKey}`, product: productKey, type: "recurring", interval: "month", interval_count: 1,
  scheme: "per_unit", currency: "USD", unit_amount: 2900, tiers: null, currency_options: { EUR: { unit_amount: 2700 } }, feature: null,
  meter_basis: "units", trial_days: 14, tax_behavior: "exclusive", active: true, metadata: {}, created_at: T0, ...over,
});

export const invoice = (id: string, over: Partial<Invoice> = {}): Invoice => ({
  id, number: `INV-${id}`, customer_id: "cus1", subject: "org:acme", subscription_id: "sub1", order_id: null, status: "paid", billing_reason: "subscription_cycle",
  currency: "USD", subtotal: 2900, discount: 0, tax: 522, total: 3422, balance_applied: 0, amount_due: 0, amount_paid: 3422, amount_refunded: 0, coupon: null,
  tax_region: "US-CA", period_start: T0, period_end: T1, attempt_count: 1, next_attempt_at: null, hosted_url: null, finalized_at: T0, paid_at: T0, fulfilled_at: T0,
  lines: [{ id: "l1", kind: "subscription", description: "Pro plan (monthly)", product: "pro", price: "pro_month", feature: null, quantity: 1, unit_amount: 2900, amount: 2900,
    discount_amount: 0, tax_rate_bp: 1800, tax_amount: 522, tax_inclusive: false, period_start: T0, period_end: T1 }],
  created_at: T0, ...over,
});

export function seedBilling(): BillingState {
  const sub: Subscription = {
    id: "sub1", customer_id: "cus1", subject: "org:acme", status: "active", currency: "USD", interval: "month", interval_count: 1, cycle_anchor: T0,
    current_period_start: "2026-11-01T00:00:00Z", current_period_end: T1, trial_ends_at: null, cancel_at_period_end: false, canceled_at: null, ended_at: null,
    cancel_reason: null, coupon: null, coupon_cycles_left: null, pending_price: null, provider: "manual", latest_invoice_id: "inv1",
    items: [{ id: "si1", price: "pro_month", product: "pro", quantity: 1 }], version: 1, metadata: {}, created_at: T0,
  };
  return {
    products: [product("pro"), product("team", { name: "Team" }), product("credits100", { name: "100 credits", kind: "credits", plan_key: null, credits: 100 })],
    prices: [
      price("pro_month", "pro"),
      price("team_month", "team", { unit_amount: 9900 }),
      price("pro_tiers", "pro", { type: "metered", scheme: "graduated", feature: "api_calls", interval: "month", tiers: [{ up_to: 100, unit_amount: 10, flat_amount: 0 }, { up_to: null, unit_amount: 5, flat_amount: 0 }], currency_options: {}, trial_days: 0 }),
      price("credits_once", "credits100", { type: "one_time", interval: null, unit_amount: 1000, currency_options: { JPY: { unit_amount: 1500 } }, trial_days: 0 }),
    ],
    customers: [
      { id: "cus1", subject: "org:acme", email: "ap@acme.test", name: "Acme", currency: "USD", region: "US-CA", tax_exempt: false, provider: "manual", provider_customer_id: null, has_payment_method: true, payment_method_label: "Visa 4242", balance: 12345, metadata: {}, created_at: T0 },
      { id: "cus2", subject: "usr_ana", email: "ana@example.test", name: null, currency: "JPY", region: null, tax_exempt: false, provider: null, provider_customer_id: null, has_payment_method: false, payment_method_label: null, balance: 0, metadata: {}, created_at: T0 },
    ],
    subscriptions: [sub, { ...sub, id: "sub2", subject: "usr_ana", customer_id: "cus2", status: "past_due", currency: "JPY", latest_invoice_id: "inv2", items: [{ id: "si2", price: "pro_month", product: "pro", quantity: 1 }] }],
    orders: [{ id: "ord1", customer_id: "cus1", subject: "org:acme", status: "paid", currency: "USD", invoice_id: "inv3", created_at: T0 }],
    invoices: [
      invoice("inv1"),
      invoice("inv2", { subject: "usr_ana", customer_id: "cus2", status: "open", currency: "JPY", subtotal: 2900, tax: 0, total: 2900, amount_due: 2900, amount_paid: 0, attempt_count: 2, next_attempt_at: "2026-11-12T00:00:00Z", paid_at: null }),
      invoice("inv3", { status: "paid", order_id: "ord1", subscription_id: null, billing_reason: "order", total: 1000, subtotal: 1000, tax: 0, amount_paid: 1000, lines: [{ id: "l3", kind: "one_time", description: "100 credits", product: "credits100", price: "credits_once", feature: null, quantity: 1, unit_amount: 1000, amount: 1000, discount_amount: 0, tax_rate_bp: 0, tax_amount: 0, tax_inclusive: false, period_start: null, period_end: null }] }),
    ],
    payments: [
      { id: "pay1", invoice_id: "inv1", subject: "org:acme", provider: "manual", provider_ref: null, status: "succeeded", amount: 3422, currency: "USD", amount_refunded: 0, failure_code: null, failure_message: null, attempt: 1, created_at: T0, updated_at: T0 },
      { id: "pay2", invoice_id: "inv2", subject: "usr_ana", provider: "stripe", provider_ref: "pi_1", status: "failed", amount: 2900, currency: "JPY", amount_refunded: 0, failure_code: "card_declined", failure_message: "Your card was declined", attempt: 2, created_at: T0, updated_at: T0 },
    ],
    refunds: [{ id: "ref1", payment_id: "pay1", invoice_id: "inv1", credit_note_id: "cn1", provider: "manual", provider_ref: null, status: "succeeded", amount: 500, currency: "USD", reason: "goodwill", created_at: T0 }],
    notes: [{ id: "cn1", number: "CN-1", invoice_id: "inv1", subject: "org:acme", currency: "USD", amount: 500, refund_amount: 500, balance_amount: 0, reason: "goodwill", refund: null, credits_reversed: 0, created_at: T0 }],
    coupons: [{ id: "cp1", code: "WELCOME15", name: null, kind: "percent", percent_bp: 1500, amount_off: null, currency: null, duration: "once", duration_cycles: null, max_redemptions: 100, redeemed: 3, expires_at: null, products: [], active: true }],
    providers: [{ provider: "stripe", key_id: null, hint: "…c4f2", api_base: null, enabled: true, webhook_path: "/api/v1/billing/webhooks/stripe", updated_at: T0, test_mode: true, environment: "test", config: {}, secrets_set: ["api_key", "webhook_secret"] }],
    secrets: {},
    storeProducts: [
      { provider: "app_store", product_id: "com.acme.pro.monthly", price: "team_month", kind: "subscription", updated_at: T0 },
      { provider: "google_play", product_id: "coins_100", price: "credits_once", kind: "one_time", updated_at: T0 },
    ],
    storePurchases: [
      { id: "sp1", provider: "app_store", purchase_id: "orig-1001", subject: "usr_ana", status: "active", kind: "subscription", price: "team_month", product_id: "com.acme.pro.monthly", environment: "test", auto_renew: true, expires_at: T1, last_transaction_id: "tx-9", order_id: null, subscription_id: "sub1", created_at: T0, updated_at: T0 },
      { id: "sp2", provider: "google_play", purchase_id: "tok-2", subject: "org:acme", status: "grace", kind: "subscription", price: "team_month", product_id: "pro", environment: "test", auto_renew: false, expires_at: T1, created_at: T0, updated_at: T0 },
      { id: "sp3", provider: "creem", purchase_id: "ord-3", subject: "usr_ana", status: "refunded", kind: "one_time", price: "credits_once", product_id: "prod_c", environment: "test", auto_renew: false, order_id: "ord1", created_at: T0, updated_at: T0 },
    ],
    settings: { default_currency: "USD", retry_days: [1, 3, 5], final_action: "cancel", invoice_prefix: "INV", credit_note_prefix: "CN", incomplete_days: 1, webhook_max_age_secs: 259200, webhook_tolerance_secs: 300 },
    webhooks: [{ id: "we1", provider: "stripe", event_id: "evt_1", event_type: "invoice.paid", event_at: T0, received_at: T0, status: "processed", result: "applied", attempts: 1 }],
    deliveries: seedDeliveries(),
    replayAs: "applied", reverifyAs: "applied", calls: { replay: [], reverify: [] },
    usage: [
      { feature: "api_calls", kind: "metered", scope_key: "", enabled: true, limit: 1000, carried_in: 0, used: 750, reserved: 0, remaining: 250, pack_remaining: 0, percent: 75, enforcement: "hard", period: "month", window_start: T0, window_end: T1 },
      { feature: "exports", kind: "metered", scope_key: "", enabled: true, limit: 100, carried_in: 0, used: 95, reserved: 0, remaining: 5, pack_remaining: 50, percent: 95, enforcement: "soft", period: "month", window_start: T0, window_end: T1 },
      { feature: "seats", kind: "gauge", scope_key: "", enabled: true, limit: 10, carried_in: 0, used: 10, reserved: 0, remaining: 0, pack_remaining: 0, percent: 100, enforcement: "hard", period: "none", window_start: T0, window_end: null },
      { feature: "storage", kind: "gauge", scope_key: "", enabled: true, limit: 100, carried_in: 0, used: 10, reserved: 0, remaining: 90, pack_remaining: 0, percent: 10, enforcement: "hard", period: "none", window_start: T0, window_end: null },
    ],
    ledger: [
      { seq: 2, id: "le2", at: T0, subject: "org:acme", kind: "consumption", amount: -40, balance_after: 960, lot_id: "lot1", ref_entry: null, feature: "ai_generation", units: 4, usage_event_id: null, usage_seq: 7, expires_at: null, idempotency_key: "k2", actor: "system", reason: null, external_ref: null, request_id: null, metadata: {} },
      { seq: 1, id: "le1", at: T0, subject: "org:acme", kind: "top_up", amount: 1000, balance_after: 1000, lot_id: "lot1", ref_entry: null, feature: null, units: null, usage_event_id: null, usage_seq: null, expires_at: null, idempotency_key: "k1", actor: "admin", reason: "launch", external_ref: null, request_id: null, metadata: {} },
    ],
    creditBalance: 960,
    taxRates: [{ id: "tx1", region: "IN", name: "GST", rate_bp: 1800, active: true }],
  };
}

const delivery = (id: string, over: Partial<WebhookDeliveryDetail> = {}): WebhookDeliveryDetail => ({
  id, provider: "stripe", account: null, received_at: T0, client_addr: "203.0.113.7", body_size: 64, body_stored: true, body_truncated: false, body_sha256: "ab".repeat(32),
  verification: "verified", reason: null, event_id: `evt_${id}`, event_type: "invoice.paid", processing: "applied", outcome: "applied", detail: "invoice_paid", links: {},
  latency_us: 4200, replays: 0, reverify_attempts: 0, reverified_at: null, updated_at: T0,
  headers: { "content-type": "application/json", "stripe-signature": "t=1,v1=abc" }, body: '{"id":"evt_1","type":"invoice.paid"}', body_encoding: "utf8", ...over,
});
function seedDeliveries(): WebhookDeliveryDetail[] {
  return [
    delivery("whd_1", { links: { webhook_event_id: "we1", customer_id: "cus1", subscription_id: "sub1", invoice_id: "inv1", payment_id: "pay1" } }),
    delivery("whd_2", { verification: "signature_invalid", reason: "signature_mismatch", event_id: null, event_type: null, processing: null, outcome: "signature_invalid", detail: null, latency_us: 300, body: '{"id":"evt_9","type":"invoice.paid"}' }),
    delivery("whd_3", { provider: "razorpay", account: "rzp_live_1", event_type: "payment.captured", outcome: "duplicate", processing: "duplicate", detail: null }),
    delivery("whd_4", { provider: "wechat_pay", event_type: null, outcome: "failed", processing: "failed", detail: "database busy", body: "AAEC/w==", body_encoding: "base64", body_size: 4 }),
    delivery("whd_5", { verification: "rejected", reason: "live_mode", outcome: "rejected", processing: null, event_id: null, event_type: null, body: null, body_encoding: null, body_stored: false, detail: null }),
    delivery("whd_6", { provider: "google_play", verification: "malformed", reason: "bad_push", outcome: "malformed", processing: null, event_id: null, event_type: null }),
  ];
}

const list = (items: unknown[]) => ({ status: 200, body: { items } });
const missing = (what: string) => envelope(404, "not_found", `no such ${what}`);

export function installBilling(mock: MockApi, opts: { disabled?: boolean } = {}): BillingState {
  const st = seedBilling();
  const B = "/api/v1/billing";
  const C = "/api/v1/credits";
  const on = (method: string, template: string, h: Parameters<MockApi["on"]>[2]) =>
    mock.on(method, template, opts.disabled && template.startsWith(B) ? () => envelope(404, "not_found", "Not Found") : h);
  const bySubject = <T extends { subject: string }>(rows: T[], q: URLSearchParams) => rows.filter((r) => !q.get("subject") || r.subject === q.get("subject"));

  on("GET", `${B}/settings`, () => ({ status: 200, body: st.settings }));
  on("PUT", `${B}/settings`, ({ body }) => { st.settings = body as BillingSettings; return { status: 200, body: st.settings }; });
  on("GET", `${B}/providers`, () => list(st.providers));
  on("PUT", `${B}/providers/{p}`, ({ params, body }) => {
    const b = body as { api_key?: string | null; key_id?: string | null; webhook_secret?: string | null; enabled: boolean; config?: Record<string, unknown> | null; api_base?: string | null; clear?: string[] };
    // Same merge as the server: an omitted or blank secret, key id, API base or config field keeps its stored value; `clear` removes one.
    const old = st.providers.find((a) => a.provider === params.p);
    const prev = st.secrets[params.p!] ?? {};
    const clear = new Set(b.clear ?? []);
    const blank = (v: unknown) => v == null || (typeof v === "string" && !v.trim()) || (Array.isArray(v) && v.length === 0);
    const pick = (name: string, v: string | null | undefined, was: string | null | undefined) => (!blank(v) ? v!.trim() : clear.has(name) ? undefined : was ?? undefined);
    const config: Record<string, unknown> = { ...(prev.config ?? {}) };
    for (const [k, v] of Object.entries(b.config ?? {})) if (!blank(v)) config[k] = v;
    for (const c of clear) if (c.startsWith("config.")) delete config[c.slice(7)];
    const api_key = pick("api_key", b.api_key, prev.api_key);
    const webhook_secret = pick("webhook_secret", b.webhook_secret, prev.webhook_secret);
    st.secrets[params.p!] = { api_key, webhook_secret, config };
    const hidden = ["private_key", "service_account", "platform_keys"];
    const hint = !blank(b.api_key) ? (b.api_key!.trim().length > 8 ? `…${b.api_key!.trim().slice(-4)}` : null) : clear.has("api_key") ? null : old?.hint ?? null;
    const acct: ProviderAccount = {
      provider: params.p!, key_id: pick("key_id", b.key_id, old?.key_id) ?? null, hint, api_base: pick("api_base", b.api_base, old?.api_base) ?? null, enabled: b.enabled,
      webhook_path: `${B}/webhooks/${params.p}`, updated_at: T0, test_mode: true,
      environment: params.p === "app_store" ? String(config.environment || "sandbox") : "test",
      config: Object.fromEntries(Object.entries(config).filter(([k]) => !hidden.includes(k))),
      secrets_set: [...(api_key ? ["api_key"] : []), ...(webhook_secret ? ["webhook_secret"] : []), ...hidden.filter((k) => !blank(config[k])).map((k) => `config.${k}`)],
    };
    st.providers = [...st.providers.filter((a) => a.provider !== params.p), acct];
    return { status: 200, body: acct };
  });
  on("DELETE", `${B}/providers/{p}`, ({ params }) => { st.providers = st.providers.filter((a) => a.provider !== params.p); return { status: 204 }; });
  on("GET", `${B}/store-products`, () => list(st.storeProducts));
  on("PUT", `${B}/store-products/{provider}/{product_id}`, ({ params, body }) => {
    const price = st.prices.find((p) => p.key === (body as { price: string }).price);
    if (!price) return envelope(404, "not_found", "no such price");
    const sp: StoreProduct = { provider: params.provider!, product_id: params.product_id!, price: price.key, kind: price.type === "recurring" ? "subscription" : "one_time", updated_at: T1 };
    st.storeProducts = [...st.storeProducts.filter((x) => !(x.provider === sp.provider && x.product_id === sp.product_id)), sp];
    return { status: 200, body: sp };
  });
  on("DELETE", `${B}/store-products/{provider}/{product_id}`, ({ params }) => {
    st.storeProducts = st.storeProducts.filter((x) => !(x.provider === params.provider && x.product_id === params.product_id));
    return { status: 204 };
  });
  on("GET", `${B}/store-purchases`, ({ query }) => list(bySubject(st.storePurchases, query)));
  on("GET", `${B}/products`, () => list(st.products));
  on("PUT", `${B}/products/{key}`, ({ params, body }) => {
    const p = product(params.key!, { ...(body as Partial<Product>) });
    st.products = [...st.products.filter((x) => x.key !== p.key), p];
    return { status: 200, body: p };
  });
  on("GET", `${B}/prices`, ({ query }) => list(st.prices.filter((p) => !query.get("product") || p.product === query.get("product"))));
  on("PUT", `${B}/prices/{key}`, ({ params, body }) => {
    const b = body as Partial<Price>;
    const p = price(params.key!, b.product!, { ...b, tiers: b.tiers ?? null });
    st.prices = [...st.prices.filter((x) => x.key !== p.key), p];
    return { status: 200, body: p };
  });
  on("GET", `${B}/tax-rates`, () => list(st.taxRates));
  on("PUT", `${B}/tax-rates/{region}`, ({ params, body }) => {
    const r = { id: `tx_${params.region}`, region: params.region!, ...(body as { name: string; rate_bp: number; active: boolean }) };
    st.taxRates = [...st.taxRates.filter((x) => x.region !== r.region), r];
    return { status: 200, body: r };
  });
  on("GET", `${B}/coupons`, () => list(st.coupons));
  on("PUT", `${B}/coupons/{code}`, ({ params, body }) => {
    const c = { id: `cp_${params.code}`, code: params.code!, name: null, percent_bp: null, amount_off: null, currency: null, duration_cycles: null, max_redemptions: null, redeemed: 0, expires_at: null, ...(body as object) } as Coupon;
    st.coupons = [...st.coupons.filter((x) => x.code !== c.code), c];
    return { status: 200, body: c };
  });
  on("GET", `${B}/customers`, () => list(st.customers));
  on("GET", `${B}/customer`, ({ query }) => {
    const c = st.customers.find((x) => x.subject === query.get("subject"));
    return c ? { status: 200, body: c } : missing("customer");
  });
  on("PUT", `${B}/customer`, ({ body }) => {
    const b = body as { subject: string; email?: string | null; name?: string | null; region?: string | null; currency?: string | null; tax_exempt?: boolean };
    const c = st.customers.find((x) => x.subject === b.subject);
    if (!c) return missing("customer");
    Object.assign(c, { email: b.email ?? null, name: b.name ?? null, region: b.region ?? null, tax_exempt: b.tax_exempt ?? false });
    return { status: 200, body: c };
  });
  on("GET", `${B}/subscriptions`, ({ query }) => list(bySubject(st.subscriptions, query)));
  on("POST", `${B}/subscriptions`, ({ body }) => {
    const b = body as { subject: string; items: { price: string; quantity: number }[]; trial_days?: number | null };
    const pr = st.prices.find((p) => p.key === b.items[0]!.price);
    const trial = b.trial_days ?? pr?.trial_days ?? 0;
    const sub: Subscription = { ...st.subscriptions[0]!, id: `sub${st.subscriptions.length + 1}`, subject: b.subject, status: trial > 0 ? "trialing" : "active", trial_ends_at: trial > 0 ? T1 : null, items: [{ id: "x", price: b.items[0]!.price, product: pr?.product ?? "", quantity: 1 }] };
    st.subscriptions = [sub, ...st.subscriptions];
    return { status: 201, body: { subscription: sub, order: null, invoice: null, payment_url: null, replayed: false } };
  });
  const lifecycle = (name: string, apply: (s: Subscription, body: unknown) => void, shape: "sub" | "checkout") =>
    on("POST", `${B}/subscriptions/{id}/${name}`, ({ params, body }) => {
      const s = st.subscriptions.find((x) => x.id === params.id);
      if (!s) return missing("subscription");
      apply(s, body);
      return { status: 200, body: shape === "sub" ? s : { subscription: s, order: null, invoice: null, payment_url: null, replayed: false } };
    });
  lifecycle("pause", (s) => { s.status = "paused"; }, "sub");
  lifecycle("resume", (s) => { s.status = "active"; s.cancel_at_period_end = false; }, "checkout");
  lifecycle("cancel", (s, b) => { if ((b as { at_period_end: boolean }).at_period_end) s.cancel_at_period_end = true; else { s.status = "canceled"; s.ended_at = T0; } }, "checkout");
  lifecycle("change", (s, b) => { const x = b as { price: string; at_period_end: boolean }; if (x.at_period_end) s.pending_price = x.price; else s.items = [{ ...s.items[0]!, price: x.price }]; }, "checkout");
  on("GET", `${B}/admin/orders`, ({ query }) => {
    const rows = bySubject(st.orders, query).filter((o) => !query.get("status") || o.status === query.get("status"));
    const limit = Number(query.get("limit") ?? 50);
    const offset = Number(query.get("offset") ?? 0);
    return { status: 200, body: { items: rows.slice(offset, offset + limit), total: rows.length, limit, offset } };
  });
  on("GET", `${B}/admin/usage-meters`, ({ query }) => {
    const meters = st.usage
      .filter((u) => u.limit !== null && (!query.get("feature") || u.feature === query.get("feature")))
      .map((u) => {
        const pct = (u.used * 100) / (u.limit as number);
        return { subject: "org:acme", feature: u.feature, scope_key: u.scope_key, used: u.used, reserved: u.reserved, carried_in: u.carried_in, limit: u.limit, percent: pct, state: pct >= 100 ? "over" : pct >= 90 ? "critical" : pct >= 70 ? "warn" : "ok", window_start: u.window_start, window_end: u.window_end ?? T1, updated_at: T0 };
      })
      .filter((m) => (!query.get("subject") || m.subject === query.get("subject")) && m.percent >= Number(query.get("min_percent") ?? 0));
    const limit = Number(query.get("limit") ?? 50);
    const offset = Number(query.get("offset") ?? 0);
    return { status: 200, body: { items: meters.slice(offset, offset + limit), total: meters.length, limit, offset } };
  });
  on("GET", `${B}/dunning`, () => {
    const items = st.invoices
      .filter((i) => i.status === "open" && (i.attempt_count > 0 || i.next_attempt_at))
      .map((i) => {
        const failed = st.payments.find((p) => p.invoice_id === i.id && p.status === "failed");
        return {
          invoice_id: i.id, number: i.number, subject: i.subject, customer_id: i.customer_id, subscription_id: i.subscription_id,
          subscription_status: st.subscriptions.find((s) => s.id === i.subscription_id)?.status ?? null, currency: i.currency, amount_due: i.amount_due,
          attempt_count: i.attempt_count, retries_left: Math.max(0, st.settings.retry_days.length - i.attempt_count), next_attempt_at: i.next_attempt_at,
          last_failure_code: failed?.failure_code ?? null, last_failure_message: failed?.failure_message ?? null, final_action: st.settings.final_action, created_at: i.created_at,
        };
      });
    return list(items);
  });
  on("POST", `${B}/subscriptions/{id}/change/preview`, ({ params, body }) => {
    const s = st.subscriptions.find((x) => x.id === params.id);
    if (!s) return missing("subscription");
    const b = body as { price: string; proration?: string | null; at_period_end: boolean };
    const from = st.prices.find((p) => p.key === s.items[0]!.price)!;
    const to = st.prices.find((p) => p.key === b.price)!;
    const proration = b.proration ?? "create_prorations";
    const half = (n: number) => Math.round(n / 2);
    const now = !b.at_period_end && proration !== "none";
    const credit = now ? -half(from.unit_amount) : 0;
    const charge = now ? half(to.unit_amount) : 0;
    const lines = now
      ? [{ kind: "proration", description: `Unused time on ${from.key}`, price: from.key, quantity: 1, amount: credit }, { kind: "proration", description: `Remaining time on ${to.key}`, price: to.key, quantity: 1, amount: charge }]
      : [];
    return {
      status: 200,
      body: {
        subscription_id: s.id, currency: s.currency, from_price: from.key, to_price: to.key, quantity: 1, proration, at_period_end: b.at_period_end,
        effective_at: b.at_period_end ? s.current_period_end : T0, period_start: s.current_period_start, period_end: s.current_period_end, lines,
        credit, charge, net: credit + charge, invoiced_now: proration === "always_invoice" && now, new_period_amount: to.unit_amount,
      },
    };
  });
  const archive = <T extends { active: boolean }>(template: string, find: (key: string) => T | undefined) =>
    on("POST", template, ({ params }) => { const x = find(Object.values(params)[0]!); if (!x) return missing("item"); x.active = false; return { status: 200, body: x }; });
  archive(`${B}/products/{key}/archive`, (k) => st.products.find((p) => p.key === k));
  archive(`${B}/prices/{key}/archive`, (k) => st.prices.find((p) => p.key === k));
  archive(`${B}/coupons/{code}/archive`, (k) => st.coupons.find((c) => c.code === k));
  archive(`${B}/tax-rates/{region}/archive`, (k) => st.taxRates.find((r) => r.region === k));
  on("GET", `${B}/orders`, ({ query }) => list(bySubject(st.orders, query)));
  on("POST", `${B}/orders`, ({ body }) => {
    const b = body as { subject?: string };
    const o: Order = { id: `ord${st.orders.length + 1}`, customer_id: "cus1", subject: b.subject ?? "org:acme", status: "open", currency: "USD", invoice_id: null, created_at: T0 };
    st.orders = [o, ...st.orders];
    return { status: 201, body: { subscription: null, order: o, invoice: null, payment_url: null, replayed: false } };
  });
  on("GET", `${B}/invoices`, ({ query }) => list(bySubject(st.invoices, query).filter((i) => !query.get("status") || i.status === query.get("status"))));
  on("GET", `${B}/invoices/{id}`, ({ params }) => { const i = st.invoices.find((x) => x.id === params.id); return i ? { status: 200, body: i } : missing("invoice"); });
  on("POST", `${B}/invoices/{id}/pay`, ({ params }) => {
    const i = st.invoices.find((x) => x.id === params.id)!;
    i.attempt_count += 1;
    return { status: 200, body: { invoice: i, payment: null, payment_url: null } };
  });
  on("POST", `${B}/invoices/{id}/mark-paid`, ({ params }) => { const i = st.invoices.find((x) => x.id === params.id)!; Object.assign(i, { status: "paid", amount_paid: i.total, amount_due: 0 }); return { status: 200, body: i }; });
  on("POST", `${B}/invoices/{id}/void`, ({ params }) => { const i = st.invoices.find((x) => x.id === params.id)!; i.status = "void"; return { status: 200, body: i }; });
  on("POST", `${B}/invoices/{id}/credit-notes`, ({ params, body }) => {
    const b = body as { amount: number; to_balance: boolean; reason?: string };
    const i = st.invoices.find((x) => x.id === params.id)!;
    if (!b.to_balance) i.amount_refunded += b.amount;
    const n: CreditNote = { id: `cn${st.notes.length + 1}`, number: `CN-${st.notes.length + 1}`, invoice_id: i.id, subject: i.subject, currency: i.currency, amount: b.amount, refund_amount: b.to_balance ? 0 : b.amount, balance_amount: b.to_balance ? b.amount : 0, reason: b.reason ?? null, refund: null, credits_reversed: 0, created_at: T0 };
    st.notes = [n, ...st.notes];
    return { status: 201, body: n };
  });
  on("GET", `${B}/credit-notes`, ({ query }) => list(bySubject(st.notes, query)));
  on("GET", `${B}/payments`, ({ query }) => list(bySubject(st.payments, query)));
  on("GET", `${B}/refunds`, () => list(st.refunds));
  on("GET", `${B}/webhook-events`, () => list(st.webhooks));
  on("GET", `${B}/webhook-deliveries`, ({ query }) => {
    const f = (k: string) => query.get(k) ?? "";
    const rows = st.deliveries.filter((d) => (!f("provider") || d.provider === f("provider")) && (!f("outcome") || d.outcome === f("outcome")) && (!f("event_type") || d.event_type === f("event_type")) && (!f("from") || d.received_at >= f("from")) && (!f("to") || d.received_at < f("to")));
    const limit = Number(f("limit") || 50), offset = Number(f("offset") || 0);
    const items = rows.slice(offset, offset + limit).map((d) => { const { headers: _h, body: _b, body_encoding: _e, ...rest } = d; void _h; void _b; void _e; return rest; });
    return { status: 200, body: { items, total: rows.length, limit, offset } };
  });
  on("GET", `${B}/webhook-deliveries/{id}`, ({ params }) => { const d = st.deliveries.find((x) => x.id === params.id); return d ? { status: 200, body: d } : missing("delivery"); });
  on("POST", `${B}/webhook-deliveries/{id}/replay`, ({ params }) => {
    const d = st.deliveries.find((x) => x.id === params.id);
    if (!d) return missing("delivery");
    if (d.verification !== "verified") return envelope(409, "delivery_not_verified", "not verified");
    if (!d.body_stored) return envelope(409, "body_not_stored", "no body");
    st.calls.replay.push(d.id);
    d.replays++;
    return { status: 200, body: { result: st.replayAs, detail: st.replayAs === "applied" ? "invoice_paid" : null, delivery: d } };
  });
  on("POST", `${B}/webhook-deliveries/{id}/reverify`, ({ params }) => {
    const d = st.deliveries.find((x) => x.id === params.id);
    if (!d) return missing("delivery");
    if (d.verification === "verified") return envelope(409, "delivery_already_verified", "verified");
    if (d.provider === "google_play") return envelope(409, "not_reverifiable", "no push token");
    if (!d.body_stored) return envelope(409, "body_not_stored", "no body");
    st.calls.reverify.push(d.id);
    d.reverify_attempts++;
    if (st.reverifyAs === "applied") { d.verification = "verified"; d.processing = "applied"; d.outcome = "applied"; d.reason = null; d.detail = "invoice_paid"; d.reverified_at = T1; }
    return { status: 200, body: { result: st.reverifyAs, detail: st.reverifyAs === "unverified" ? "signature_mismatch" : "invoice_paid", delivery: d } };
  });
  on("POST", `${B}/maintenance`, () => ({ status: 200, body: { usage_events_read: 0, usage_recorded: 0, trials_ended: 0, renewed: 0, ended: 0, retries: 1, fulfilled: 0, synced: 0 } }));
  on("GET", `${B}/stats`, () => ({
    status: 200,
    body: {
      from: T0, to: T1, recurring: [{ currency: "USD", mrr: 129900, arr: 1558800, subscriptions: 12 }, { currency: "JPY", mrr: 500000, arr: 6000000, subscriptions: 4 }],
      by_status: { active: 12, trialing: 2, past_due: 1 }, active_at_start: 10, new_subscriptions: 4, churned: 1, churn_rate_bp: 1000,
      revenue: [{ start: T0, currency: "USD", gross: 300000, refunded: 5000, net: 295000, payments: 9 }, { start: T0, currency: "JPY", gross: 90000, refunded: 0, net: 90000, payments: 3 }],
      failed_payments: 2,
    },
  }));

  // Entitlements side: usage meters and the catalog lists.
  mock.on("GET", "/api/v1/usage", () => list(st.usage));
  mock.on("GET", "/api/v1/usage/alerts", () => list([{ id: "al1", at: T0, subject: "org:acme", feature: "exports", scope_key: "", window_start: T0, threshold: 90, used: 90, limit: 100, delivered_at: null }]));
  mock.on("GET", "/api/v1/plans", () => list([{ key: "pro", name: "Pro", description: "", is_default: false, active: true, trial_days: 14, features: [] }]));
  mock.on("GET", "/api/v1/features", () => list([{ key: "api_calls", name: "API calls", description: "", kind: "metered", unit: "requests", permissions: [] }]));
  mock.on("GET", "/api/v1/addons", () => list([{ key: "sso", name: "SSO", description: "", active: true, features: [] }]));
  mock.on("GET", "/api/v1/usage-packs", () => list([{ key: "pack1k", name: "1k calls", feature: "api_calls", units: 1000, valid_days: 30, active: true }]));

  // Credits.
  mock.on("GET", `${C}/balance`, ({ query }) => ({ status: 200, body: { subject: query.get("subject"), balance: st.creditBalance, expiring: [], updated_at: T0 } }));
  mock.on("GET", `${C}/lots`, () => list([{ id: "lot1", subject: "org:acme", entry_id: "le1", kind: "top_up", amount: 1000, remaining: 960, expires_at: null, created_at: T0 }]));
  mock.on("GET", `${C}/ledger`, ({ query }) => ({ status: 200, body: { items: st.ledger.filter((e) => !query.get("kind") || e.kind === query.get("kind")), next: null } }));
  mock.on("GET", `${C}/audit`, ({ query }) => ({ status: 200, body: { subject: query.get("subject"), balance: 960, ledger_sum: 960, lots_remaining: 960, last_balance_after: 960, lots_match_allocations: true, entries_match_allocations: true, consistent: true } }));
  const post = (kind: "top_up" | "grant") => ({ body }: { body: unknown }) => {
    const b = body as { subject: string; amount: number; reason?: string };
    st.creditBalance += b.amount;
    const entry: LedgerEntry = { ...st.ledger[1]!, seq: st.ledger.length + 1, id: `le${st.ledger.length + 1}`, kind, amount: b.amount, balance_after: st.creditBalance, reason: b.reason ?? null };
    st.ledger = [entry, ...st.ledger];
    return { status: 201, body: { entry, replayed: false } };
  };
  mock.on("POST", `${C}/top-ups`, post("top_up"));
  mock.on("POST", `${C}/grants`, post("grant"));
  mock.on("GET", `${C}/rates`, () => list([{ feature: "ai_generation", credits_per_unit: 10, per_units: 1, basis: "units", created_at: T0, updated_at: T0 }]));
  mock.on("GET", `${C}/stats/summary`, () => ({ status: 200, body: { subject: null, from: T0, to: T1, top_up: 5000, grant: 200, consumption: 1200, spend: 0, refund: 50, reversal: 0, expiry: 10, net: 4040, balance: 4040, shortfall: 25 } }));
  mock.on("GET", `${C}/stats/consumption`, () => ({ status: 200, body: { subject: null, interval: "day", from: T0, to: T1, rows: [{ start: T0, feature: "ai_generation", debited: 1200, refunded: 50, net: 1150, units: 120, entries: 30 }], total_net: 1150 } }));
  return st;
}
