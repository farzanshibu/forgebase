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

  /** Store purchases of `subject` (default: the caller). */
  async purchases(subject?: string): Promise<StorePurchase[]> {
    return (await this.t.request<{ items: StorePurchase[] }>("/api/v1/billing/store-purchases", { query: { subject } })).items;
  }
}

/** Billing helpers: `client.billing`. */
export class BillingApi {
  readonly store: BillingStoreApi;
  constructor(t: Transport) {
    this.store = new BillingStoreApi(t);
  }
}
