import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Billing beyond the stores (`/api/v1/billing/*`, FB-70): customers, subscriptions, orders, invoices, the catalog,
 * providers, settings and the operator views. Reached through `client.billing`.
 *
 * Inputs are camelCase and map to the server's snake_case JSON one level deep: nested objects (`items`, `tiers`,
 * `currency_options`, `metadata`) keep the wire shape. Replies are the server's JSON as is.
 */

type CamelCase<S extends string> = S extends `${infer H}_${infer T}` ? `${H}${Capitalize<CamelCase<T>>}` : S;
/** Top-level keys of a wire type in camelCase. */
type Camelize<T> = { [K in keyof T as CamelCase<K & string>]: T[K] };

const seg = encodeURIComponent;

/** camelCase keys to snake_case, one level deep; `undefined` values are left out. */
const wire = (o: object): Record<string, unknown> =>
  Object.fromEntries(
    Object.entries(o)
      .filter(([, v]) => v !== undefined)
      .map(([k, v]) => [k.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`), v]),
  );

const isoTime = (v: string | Date | undefined): string | undefined => (v instanceof Date ? v.toISOString() : v);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type BillingWebhookDelivery = Schemas["WebhookDelivery"];
export type BillingWebhookDeliveryDetail = Schemas["WebhookDeliveryDetail"];
export type BillingWebhookDeliveryPage = Schemas["WebhookDeliveryPage"];
/** What a replay or a re-verification did: the delivery as it is now, the `result` (`applied`, `duplicate`, `rejected`, ...) and a `detail`. */
export type BillingWebhookDeliveryResult = Schemas["WebhookDeliveryResult"];

/** Filters of `billing.webhookDeliveries.list`. */
export interface BillingWebhookDeliveriesQuery {
  /** `stripe`, `razorpay`, `wechatpay`, ... */
  provider?: string;
  /** `applied`, `duplicate`, `rejected`, `unverified`, ... as the server reports them. */
  outcome?: string;
  eventType?: string;
  eventId?: string;
  /** Received at or after this time. */
  from?: string | Date;
  /** Received before this time. */
  to?: string | Date;
  limit?: number;
  offset?: number;
}

export type BillingCustomer = Schemas["Customer"];
export type BillingCustomerInput = Camelize<Schemas["CustomerInput"]>;
export type BillingSubscription = Schemas["BillingSubscription"];
export type BillingSubscriptionStatus = Schemas["BillingSubscriptionStatus"];
export type BillingSubscribeInput = Camelize<Schemas["SubscribeInput"]>;
export type BillingChangePlanInput = Camelize<Schemas["ChangePlanInput"]>;
export type BillingCancelInput = Camelize<Schemas["CancelInput"]>;
/** The result of subscribing, changing plan, cancelling or resuming. `payment_url` is set when the customer still has to pay. */
export type BillingCheckout = Schemas["Checkout"];
export type BillingProrationPreview = Schemas["ProrationPreview"];
export type BillingOrder = Schemas["Order"];
export type BillingOrderInput = Camelize<Schemas["OrderInput"]>;
export type BillingOrderPage = Schemas["OrderPage"];
export type BillingInvoice = Schemas["Invoice"];
export type BillingInvoiceStatus = Schemas["InvoiceStatus"];
export type BillingPayment = Schemas["Payment"];
export type BillingRefund = Schemas["Refund"];
export type BillingCreditNote = Schemas["CreditNote"];
export type BillingCreditNoteInput = Camelize<Schemas["CreditNoteInput"]>;
/** What paying an invoice did: the invoice, the payment, and the hosted page URL when the customer must pay. */
export type BillingPaymentCollection = Schemas["BillingCollection"];
export type BillingPortal = Schemas["Portal"];
export type BillingPaymentMethodInput = Camelize<Schemas["PaymentMethodInput"]>;
export type BillingProduct = Schemas["Product"];
export type BillingProductInput = Camelize<Schemas["ProductInput"]>;
export type BillingPrice = Schemas["Price"];
export type BillingPriceInput = Camelize<Schemas["PriceInput"]>;
export type BillingCoupon = Schemas["Coupon"];
export type BillingCouponInput = Camelize<Schemas["CouponInput"]>;
export type BillingTaxRate = Schemas["TaxRate"];
export type BillingTaxRateInput = Camelize<Schemas["TaxRateInput"]>;
export type BillingProvider = Schemas["ProviderAccount"];
export type BillingProviderInput = Camelize<Schemas["ProviderInput"]>;
export type BillingSettings = Schemas["BillingSettings"];
/** The settings in camelCase, as `settings.put` takes them (the whole object: read, change, write back). */
export type BillingSettingsInput = Camelize<Schemas["BillingSettings"]>;
export type BillingStats = Schemas["BillingStats"];
export type BillingMaintenanceReport = Schemas["BillingMaintenanceReport"];
export type BillingUsageMeter = Schemas["UsageMeter"];
export type BillingUsageMeterPage = Schemas["UsageMeterPage"];
export type BillingDunningEntry = Schemas["DunningEntry"];

/** List filters shared by the per-subject lists. */
export interface BillingSubjectListOptions {
  /** `org:<id>` or a principal key (default: the caller's subject). */
  subject?: string;
  /** Rows to return. */
  limit?: number;
}

export interface BillingListInvoicesOptions extends BillingSubjectListOptions {
  /** Only this status (`draft`, `open`, `paid`, `void`, ...). */
  status?: BillingInvoiceStatus;
}

export interface BillingListAllOrdersOptions {
  subject?: string;
  /** Only this order status. */
  status?: string;
  /** Page size, 1-500 (default 50). */
  limit?: number;
  offset?: number;
}

export interface BillingUsageMetersOptions {
  subject?: string;
  feature?: string;
  /** Only meters at or above this percentage of their limit. */
  minPercent?: number;
  limit?: number;
  offset?: number;
}

export interface BillingStatsOptions {
  /** Window start (default: 30 days before `to`). */
  from?: string | Date;
  /** Window end (default: now). */
  to?: string | Date;
  /** Bucket size of the revenue series (default `day`). */
  interval?: "day" | "month";
}

// ---------------------------------------------------------------------------
// Customers, portal
// ---------------------------------------------------------------------------

/** The billing customer of a subject: `client.billing.customers`. */
export class BillingCustomersApi {
  constructor(private readonly t: Transport) {}

  /** The customer of `subject` (default: the caller's). */
  get(subject?: string): Promise<BillingCustomer> {
    return this.t.request("/api/v1/billing/customer", { query: { subject } });
  }

  /** Creates or updates the customer of a subject: billing email, currency, tax region, provider. */
  put(input: BillingCustomerInput): Promise<BillingCustomer> {
    return this.t.request("/api/v1/billing/customer", { method: "PUT", body: wire(input) });
  }

  /** Every customer (realm-level `billing:read`). `limit` is at most 1000. */
  async list(limit?: number): Promise<BillingCustomer[]> {
    return (await this.t.request<{ items: BillingCustomer[] }>("/api/v1/billing/customers", { query: { limit } })).items;
  }
}

/** What a customer sees and does: `client.billing.portal`. */
export class BillingPortalApi {
  constructor(private readonly t: Transport) {}

  /** The customer, live subscriptions and recent invoices, payments and credit notes of `subject`. */
  get(subject?: string): Promise<BillingPortal> {
    return this.t.request("/api/v1/billing/portal", { query: { subject } });
  }

  /** A provider-hosted page that saves or replaces the payment method: send the customer to `url`. */
  paymentMethod(input: BillingPaymentMethodInput = {}): Promise<Schemas["HostedPage"]> {
    return this.t.request("/api/v1/billing/portal/payment-method", { method: "POST", body: wire(input) });
  }
}

// ---------------------------------------------------------------------------
// Subscriptions, orders
// ---------------------------------------------------------------------------

/** Recurring subscriptions: `client.billing.subscriptions`. */
export class BillingSubscriptionsApi {
  constructor(private readonly t: Transport) {}

  /** Subscriptions of `subject` (default: the caller's). */
  async list(subject?: string): Promise<BillingSubscription[]> {
    return (await this.t.request<{ items: BillingSubscription[] }>("/api/v1/billing/subscriptions", { query: { subject } })).items;
  }

  get(id: string): Promise<BillingSubscription> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}`);
  }

  /** Subscribes: a trial, or the first invoice collected (`payment_url` when the customer must pay). A replayed `idempotencyKey` returns the first result with `replayed: true`; a second live subscription is a `ConflictError`. */
  create(input: BillingSubscribeInput): Promise<BillingCheckout> {
    return this.t.request("/api/v1/billing/subscriptions", { method: "POST", body: wire(input) });
  }

  /** Upgrades or downgrades with proration. */
  change(id: string, input: BillingChangePlanInput): Promise<BillingCheckout> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}/change`, { method: "POST", body: wire(input) });
  }

  /** What `change` would do (proration lines, credit, charge, net) with no side effects. */
  previewChange(id: string, input: BillingChangePlanInput): Promise<BillingProrationPreview> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}/change/preview`, { method: "POST", body: wire(input) });
  }

  /** Cancels at the period end (default) or now (`atPeriodEnd: false`). */
  cancel(id: string, input: BillingCancelInput = {}): Promise<BillingCheckout> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}/cancel`, { method: "POST", body: wire(input) });
  }

  /** Pauses an active subscription. */
  pause(id: string): Promise<BillingSubscription> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}/pause`, { method: "POST" });
  }

  /** Resumes a paused subscription or withdraws a scheduled cancellation. */
  resume(id: string): Promise<BillingCheckout> {
    return this.t.request(`/api/v1/billing/subscriptions/${seg(id)}/resume`, { method: "POST" });
  }
}

/** One-time purchases (credit top-ups, usage packs): `client.billing.orders`. */
export class BillingOrdersApi {
  constructor(private readonly t: Transport) {}

  /** Orders of `subject` (default: the caller's). */
  async list(subject?: string): Promise<BillingOrder[]> {
    return (await this.t.request<{ items: BillingOrder[] }>("/api/v1/billing/orders", { query: { subject } })).items;
  }

  get(id: string): Promise<BillingOrder> {
    return this.t.request(`/api/v1/billing/orders/${seg(id)}`);
  }

  /** Buys once. A replayed `idempotencyKey` returns the first result with `replayed: true`. */
  create(input: BillingOrderInput): Promise<BillingCheckout> {
    return this.t.request("/api/v1/billing/orders", { method: "POST", body: wire(input) });
  }

  /** Every order of the instance, newest first, with paging (billing readers). */
  listAll(opts: BillingListAllOrdersOptions = {}): Promise<BillingOrderPage> {
    return this.t.request("/api/v1/billing/admin/orders", {
      query: { subject: opts.subject, status: opts.status, limit: opts.limit, offset: opts.offset },
    });
  }
}

// ---------------------------------------------------------------------------
// Invoices, payments
// ---------------------------------------------------------------------------

/** Invoices: `client.billing.invoices`. */
export class BillingInvoicesApi {
  constructor(private readonly t: Transport) {}

  /** Invoices of `subject`, newest first. `limit` is at most 500. */
  async list(opts: BillingListInvoicesOptions = {}): Promise<BillingInvoice[]> {
    return (
      await this.t.request<{ items: BillingInvoice[] }>("/api/v1/billing/invoices", {
        query: { subject: opts.subject, status: opts.status, limit: opts.limit },
      })
    ).items;
  }

  /** One invoice with its lines. */
  get(id: string): Promise<BillingInvoice> {
    return this.t.request(`/api/v1/billing/invoices/${seg(id)}`);
  }

  /** Pays an open invoice now: with the saved method, else the answer carries the hosted page `payment_url`. */
  pay(id: string, opts: { returnUrl?: string } = {}): Promise<BillingPaymentCollection> {
    return this.t.request(`/api/v1/billing/invoices/${seg(id)}/pay`, { method: "POST", body: wire(opts) });
  }

  /** Voids an unpaid invoice (operators). */
  void(id: string): Promise<BillingInvoice> {
    return this.t.request(`/api/v1/billing/invoices/${seg(id)}/void`, { method: "POST" });
  }

  /** Records an out-of-band payment (operators; manual billing). */
  markPaid(id: string, opts: { reference?: string } = {}): Promise<BillingInvoice> {
    return this.t.request(`/api/v1/billing/invoices/${seg(id)}/mark-paid`, { method: "POST", body: wire(opts) });
  }

  /** Issues a credit note: a refund, or a credit to the customer balance with `toBalance` (operators). Idempotent by `idempotencyKey`. */
  creditNote(id: string, input: BillingCreditNoteInput): Promise<BillingCreditNote> {
    return this.t.request(`/api/v1/billing/invoices/${seg(id)}/credit-notes`, { method: "POST", body: wire(input) });
  }
}

/** A read-only list of a subject's records (credit notes, payments, refunds). */
export class BillingSubjectList<T> {
  constructor(
    private readonly t: Transport,
    private readonly path: string,
  ) {}

  /** Records of `subject` (default: the caller's), newest first. */
  async list(opts: BillingSubjectListOptions = {}): Promise<T[]> {
    return (await this.t.request<{ items: T[] }>(this.path, { query: { subject: opts.subject, limit: opts.limit } })).items;
  }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/** Products: `client.billing.catalog.products`. Writes are for platform administrators. */
export class BillingProductsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<BillingProduct[]> {
    return (await this.t.request<{ items: BillingProduct[] }>("/api/v1/billing/products")).items;
  }

  get(key: string): Promise<BillingProduct> {
    return this.t.request(`/api/v1/billing/products/${seg(key)}`);
  }

  /** Creates or replaces a product. Its `kind` maps a payment to a plan, add-on, usage pack or credits. */
  put(key: string, input: BillingProductInput): Promise<BillingProduct> {
    return this.t.request(`/api/v1/billing/products/${seg(key)}`, { method: "PUT", body: wire(input) });
  }

  /** Archives a product (idempotent): it can no longer be bought; existing subscriptions continue. */
  archive(key: string): Promise<BillingProduct> {
    return this.t.request(`/api/v1/billing/products/${seg(key)}/archive`, { method: "POST" });
  }
}

/** Prices: `client.billing.catalog.prices`. Writes are for platform administrators. */
export class BillingPricesApi {
  constructor(private readonly t: Transport) {}

  /** Prices, of one product when `product` is given. */
  async list(product?: string): Promise<BillingPrice[]> {
    return (await this.t.request<{ items: BillingPrice[] }>("/api/v1/billing/prices", { query: { product } })).items;
  }

  get(key: string): Promise<BillingPrice> {
    return this.t.request(`/api/v1/billing/prices/${seg(key)}`);
  }

  /** Creates or replaces a price: recurring, one-time or metered; per unit, graduated or volume tiers; several currencies. Changing the amounts of a price in use is a `ConflictError`. */
  put(key: string, input: BillingPriceInput): Promise<BillingPrice> {
    return this.t.request(`/api/v1/billing/prices/${seg(key)}`, { method: "PUT", body: wire(input) });
  }

  /** Archives a price (idempotent): new subscriptions and plan changes refuse it; existing ones keep renewing on it. */
  archive(key: string): Promise<BillingPrice> {
    return this.t.request(`/api/v1/billing/prices/${seg(key)}/archive`, { method: "POST" });
  }
}

/** Coupons: `client.billing.catalog.coupons` (platform administrators). */
export class BillingCouponsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<BillingCoupon[]> {
    return (await this.t.request<{ items: BillingCoupon[] }>("/api/v1/billing/coupons")).items;
  }

  /** Creates or replaces a coupon. */
  put(code: string, input: BillingCouponInput): Promise<BillingCoupon> {
    return this.t.request(`/api/v1/billing/coupons/${seg(code)}`, { method: "PUT", body: wire(input) });
  }

  /** Archives a coupon (idempotent): it cannot be redeemed again. */
  archive(code: string): Promise<BillingCoupon> {
    return this.t.request(`/api/v1/billing/coupons/${seg(code)}/archive`, { method: "POST" });
  }
}

/** Tax rates per region: `client.billing.catalog.taxRates`. */
export class BillingTaxRatesApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<BillingTaxRate[]> {
    return (await this.t.request<{ items: BillingTaxRate[] }>("/api/v1/billing/tax-rates")).items;
  }

  /** Sets the rate of a region (`IN`, `DE`, `US-CA`, ...; `rateBp` in basis points). Platform administrators. */
  put(region: string, input: BillingTaxRateInput): Promise<BillingTaxRate> {
    return this.t.request(`/api/v1/billing/tax-rates/${seg(region)}`, { method: "PUT", body: wire(input) });
  }

  /** Archives the rate of a region (idempotent): no tax is added for it afterwards. */
  archive(region: string): Promise<BillingTaxRate> {
    return this.t.request(`/api/v1/billing/tax-rates/${seg(region)}/archive`, { method: "POST" });
  }
}

/** Products, prices, coupons and tax rates: `client.billing.catalog`. */
export class BillingCatalogApi {
  readonly products: BillingProductsApi;
  readonly prices: BillingPricesApi;
  readonly coupons: BillingCouponsApi;
  readonly taxRates: BillingTaxRatesApi;
  constructor(t: Transport) {
    this.products = new BillingProductsApi(t);
    this.prices = new BillingPricesApi(t);
    this.coupons = new BillingCouponsApi(t);
    this.taxRates = new BillingTaxRatesApi(t);
  }
}

// ---------------------------------------------------------------------------
// Providers, settings, operator views
// ---------------------------------------------------------------------------

/** Payment provider accounts: `client.billing.providers`. Secrets are sealed and never returned. */
export class BillingProvidersApi {
  constructor(private readonly t: Transport) {}

  /** Configured providers (no secrets). */
  async list(): Promise<BillingProvider[]> {
    return (await this.t.request<{ items: BillingProvider[] }>("/api/v1/billing/providers")).items;
  }

  /** Saves a provider account (`stripe`, `razorpay`, `wechat_pay`, `manual`, or a store: `app_store`, `google_play`, `creem` with its `config`). Test-mode credentials only; `clear` names stored secrets to remove. */
  put(provider: string, input: BillingProviderInput): Promise<BillingProvider> {
    return this.t.request(`/api/v1/billing/providers/${seg(provider)}`, { method: "PUT", body: wire(input) });
  }

  delete(provider: string): Promise<void> {
    return this.t.request(`/api/v1/billing/providers/${seg(provider)}`, { method: "DELETE" });
  }
}

/**
 * The payment-provider webhook inspector (platform administrators): every request a provider sent to
 * `/api/v1/billing/webhooks/{provider}` with how its signature verified, what was done with it, the stored body (when
 * kept) and the links to the records it touched. `client.billing.webhookDeliveries`.
 */
export class BillingWebhookDeliveriesApi {
  constructor(private readonly t: Transport) {}

  /** Deliveries, newest first, with the total for the filter. */
  list(q: BillingWebhookDeliveriesQuery = {}): Promise<BillingWebhookDeliveryPage> {
    return this.t.request("/api/v1/billing/webhook-deliveries", {
      query: { provider: q.provider, outcome: q.outcome, event_type: q.eventType, event_id: q.eventId, from: isoTime(q.from), to: isoTime(q.to), limit: q.limit, offset: q.offset },
    });
  }

  /** One delivery with its stored body and headers (`NotFoundError` for an unknown id). */
  get(id: string): Promise<BillingWebhookDeliveryDetail> {
    return this.t.request(`/api/v1/billing/webhook-deliveries/${seg(id)}`);
  }

  /** Processes the stored body again (after a fix on your side); events that were applied before are skipped as duplicates. */
  replay(id: string): Promise<BillingWebhookDeliveryResult> {
    return this.t.request(`/api/v1/billing/webhook-deliveries/${seg(id)}/replay`, { method: "POST", body: {} });
  }

  /** Verifies the stored signature again with the provider's current secrets (after fixing a wrong secret). */
  reverify(id: string): Promise<BillingWebhookDeliveryResult> {
    return this.t.request(`/api/v1/billing/webhook-deliveries/${seg(id)}/reverify`, { method: "POST", body: {} });
  }
}

/** Operator settings (dunning schedule, number prefixes, webhook windows): `client.billing.settings`. */
export class BillingSettingsApi {
  constructor(private readonly t: Transport) {}

  get(): Promise<BillingSettings> {
    return this.t.request("/api/v1/billing/settings");
  }

  /** Replaces the settings (platform administrators): read with `get`, change, write back. */
  put(settings: BillingSettingsInput): Promise<BillingSettings> {
    return this.t.request("/api/v1/billing/settings", { method: "PUT", body: wire(settings) });
  }
}

/** Statistics, dunning and maintenance: the operator views of `client.billing`. */
export class BillingOpsApi {
  constructor(private readonly t: Transport) {}

  /** MRR, ARR, churn and revenue by currency (default: the last 30 days by day). */
  stats(opts: BillingStatsOptions = {}): Promise<BillingStats> {
    return this.t.request("/api/v1/billing/stats", {
      query: { from: isoTime(opts.from), to: isoTime(opts.to), interval: opts.interval },
    });
  }

  /** Usage counters of every subject in their current windows, most used first, with 70 / 90 / 100 % states. */
  usageMeters(opts: BillingUsageMetersOptions = {}): Promise<BillingUsageMeterPage> {
    return this.t.request("/api/v1/billing/admin/usage-meters", {
      query: { subject: opts.subject, feature: opts.feature, min_percent: opts.minPercent, limit: opts.limit, offset: opts.offset },
    });
  }

  /** Open invoices with a failed attempt or a scheduled retry, next attempt first. `limit` is 1-500 (default 200). */
  async dunning(limit?: number): Promise<BillingDunningEntry[]> {
    return (await this.t.request<{ items: BillingDunningEntry[] }>("/api/v1/billing/dunning", { query: { limit } })).items;
  }

  /** Runs maintenance now (usage, renewals, dunning, fulfillment, entitlement syncs). Platform administrators. */
  maintenance(): Promise<BillingMaintenanceReport> {
    return this.t.request("/api/v1/billing/maintenance", { method: "POST" });
  }
}
