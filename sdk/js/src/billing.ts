import {
  BillingCatalogApi, BillingCustomersApi, BillingInvoicesApi, BillingOpsApi, BillingOrdersApi, BillingPortalApi, BillingProvidersApi,
  BillingSettingsApi, BillingSubjectList, BillingSubscriptionsApi,
} from "./billing-admin";
import type {
  BillingCreditNote, BillingDunningEntry, BillingMaintenanceReport, BillingPayment, BillingRefund, BillingStats, BillingStatsOptions,
  BillingUsageMeterPage, BillingUsageMetersOptions,
} from "./billing-admin";
import type { Transport } from "./http";
import type { Schemas } from "./types";

export type StoreProvider = "app_store" | "google_play" | "creem";
export type StoreProduct = Schemas["BillingStoreProduct"];
export type StorePurchase = Schemas["BillingStorePurchase"];
export type HostedPage = Schemas["HostedPage"];

/** What the app received from the store; each store reads its own fields. */
export interface StoreReceipt {
  /** App Store: the StoreKit 2 signed transaction (JWS). */
  signedTransaction?: string;
  /** App Store: the transaction id (read back with the App Store Server API). */
  transactionId?: string;
  /** Google Play: the purchase token. */
  purchaseToken?: string;
  /** Google Play: the product id. */
  productId?: string;
  /** Google Play: `subscription` (default) or `one_time`. */
  kind?: "subscription" | "one_time";
  /** Creem: the checkout id of the success redirect. */
  checkoutId?: string;
  /** Subject the purchase is for (default: the caller's). */
  subject?: string;
}

export interface StoreCheckoutInput {
  /** Price key mapped to a store product. */
  price: string;
  /** Where the store returns to (`?checkout_id=` is appended by the store). */
  successUrl: string;
  /** Sent to the store as the request id; a replay returns the same checkout. */
  idempotencyKey: string;
  /** Subject (default: the caller's). */
  subject?: string;
}

const seg = (s: string) => encodeURIComponent(s);

/** Merchant-of-record store helpers (FB-72): `client.billing.store`. */
export class BillingStoreApi {
  constructor(private readonly t: Transport) {}

  /** Verifies a purchase server side with the store and applies it. Idempotent; a purchase bound to another account is a `ConflictError`. */
  verify(provider: StoreProvider, receipt: StoreReceipt): Promise<StorePurchase> {
    return this.t.request(`/api/v1/billing/store/${seg(provider)}/verify`, {
      method: "POST",
      body: {
        signed_transaction: receipt.signedTransaction,
        transaction_id: receipt.transactionId,
        purchase_token: receipt.purchaseToken,
        product_id: receipt.productId,
        kind: receipt.kind,
        checkout_id: receipt.checkoutId,
        subject: receipt.subject,
      },
    });
  }

  /** A hosted checkout at a store that sells on the web (Creem): open `url`, then `verify` with the returned `checkout_id`. */
  checkout(provider: StoreProvider, input: StoreCheckoutInput): Promise<HostedPage> {
    return this.t.request(`/api/v1/billing/store/${seg(provider)}/checkout`, {
      method: "POST",
      body: { price: input.price, success_url: input.successUrl, idempotency_key: input.idempotencyKey, subject: input.subject },
    });
  }

  /** Store products and the prices they are sold as. */
  async products(): Promise<StoreProduct[]> {
    return (await this.t.request<{ items: StoreProduct[] }>("/api/v1/billing/store-products")).items;
  }

  /** Maps a store product to a price key: recurring for subscriptions, one-time otherwise (platform administrators). */
  putProduct(provider: StoreProvider, productId: string, price: string): Promise<StoreProduct> {
    return this.t.request(`/api/v1/billing/store-products/${seg(provider)}/${seg(productId)}`, { method: "PUT", body: { price } });
  }

  /** Removes a store product mapping (platform administrators). */
  deleteProduct(provider: StoreProvider, productId: string): Promise<void> {
    return this.t.request(`/api/v1/billing/store-products/${seg(provider)}/${seg(productId)}`, { method: "DELETE" });
  }

  /** Store purchases of `subject` (default: the caller). */
  async purchases(subject?: string): Promise<StorePurchase[]> {
    return (await this.t.request<{ items: StorePurchase[] }>("/api/v1/billing/store-purchases", { query: { subject } })).items;
  }
}

/**
 * Billing: `client.billing`. Customers, subscriptions, orders, invoices and the portal are for customers and
 * their operators; the catalog, providers, settings and the operator views need platform administrator rights.
 * The provider webhook inspector is not part of this class.
 */
export class BillingApi {
  readonly store: BillingStoreApi;
  /** The billing customer of a subject. */
  readonly customers: BillingCustomersApi;
  /** What a customer sees: invoices, subscriptions and the hosted payment-method page. */
  readonly portal: BillingPortalApi;
  readonly subscriptions: BillingSubscriptionsApi;
  readonly orders: BillingOrdersApi;
  readonly invoices: BillingInvoicesApi;
  readonly creditNotes: BillingSubjectList<BillingCreditNote>;
  readonly payments: BillingSubjectList<BillingPayment>;
  readonly refunds: BillingSubjectList<BillingRefund>;
  /** Products, prices, coupons and tax rates. */
  readonly catalog: BillingCatalogApi;
  /** Payment provider accounts (Stripe, Razorpay, WeChat Pay, stores). */
  readonly providers: BillingProvidersApi;
  readonly settings: BillingSettingsApi;
  private readonly ops: BillingOpsApi;

  constructor(t: Transport) {
    this.store = new BillingStoreApi(t);
    this.customers = new BillingCustomersApi(t);
    this.portal = new BillingPortalApi(t);
    this.subscriptions = new BillingSubscriptionsApi(t);
    this.orders = new BillingOrdersApi(t);
    this.invoices = new BillingInvoicesApi(t);
    this.creditNotes = new BillingSubjectList(t, "/api/v1/billing/credit-notes");
    this.payments = new BillingSubjectList(t, "/api/v1/billing/payments");
    this.refunds = new BillingSubjectList(t, "/api/v1/billing/refunds");
    this.catalog = new BillingCatalogApi(t);
    this.providers = new BillingProvidersApi(t);
    this.settings = new BillingSettingsApi(t);
    this.ops = new BillingOpsApi(t);
  }

  /** MRR, ARR, churn and revenue by currency (default: the last 30 days by day). */
  stats(opts?: BillingStatsOptions): Promise<BillingStats> {
    return this.ops.stats(opts);
  }

  /** Usage counters of every subject in their current windows, most used first, with 70 / 90 / 100 % states. */
  usageMeters(opts?: BillingUsageMetersOptions): Promise<BillingUsageMeterPage> {
    return this.ops.usageMeters(opts);
  }

  /** Open invoices with a failed attempt or a scheduled retry, next attempt first. */
  dunning(limit?: number): Promise<BillingDunningEntry[]> {
    return this.ops.dunning(limit);
  }

  /** Runs maintenance now (usage, renewals, dunning, fulfillment, entitlement syncs). Platform administrators. */
  maintenance(): Promise<BillingMaintenanceReport> {
    return this.ops.maintenance();
  }
}
