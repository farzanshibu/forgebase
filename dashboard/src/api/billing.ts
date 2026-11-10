/**
 * Typed client for `/api/v1/billing`, `/api/v1/credits` and the usage side of `/api/v1/entitlements` (ADR-0021).
 * Hand-written; the OpenAPI fixture (billing enabled) describes the billing routes and the test mocks are checked against it.
 * Money is always integer minor units next to an ISO 4217 currency; never a float.
 */
import { ApiError, request, send } from "./client";

const B = "/api/v1/billing";
const C = "/api/v1/credits";
const V1 = "/api/v1";
const enc = encodeURIComponent;

export type ProductKind = "plan" | "addon" | "usage_pack" | "credits" | "service";
export type PriceType = "recurring" | "one_time" | "metered";
export type BillingInterval = "month" | "year";
export type PriceScheme = "per_unit" | "graduated" | "volume";
export type MeterBasis = "units" | "overage";
export type TaxBehavior = "exclusive" | "inclusive";
export type SubscriptionStatus = "incomplete" | "trialing" | "active" | "past_due" | "paused" | "canceled";
export type InvoiceStatus = "draft" | "open" | "paid" | "void" | "uncollectible";
export type PaymentStatus = "pending" | "succeeded" | "failed" | "canceled";
export type RefundStatus = "pending" | "succeeded" | "failed";
export type CouponKind = "percent" | "amount";
export type CouponDuration = "once" | "repeating" | "forever";
export type LineKind = "subscription" | "proration" | "usage" | "one_time" | "adjustment";
export type OrderStatus = "open" | "paid" | "canceled" | "refunded";
export type Proration = "always_invoice" | "create_prorations" | "none";
export type FinalAction = "cancel" | "leave_past_due";

export const PRODUCT_KINDS: ProductKind[] = ["plan", "addon", "usage_pack", "credits", "service"];
export const PRICE_TYPES: PriceType[] = ["recurring", "one_time", "metered"];
export const SCHEMES: PriceScheme[] = ["per_unit", "graduated", "volume"];
export const SUBSCRIPTION_STATUSES: SubscriptionStatus[] = ["incomplete", "trialing", "active", "past_due", "paused", "canceled"];
export const INVOICE_STATUSES: InvoiceStatus[] = ["draft", "open", "paid", "void", "uncollectible"];

export interface Tier { up_to?: number | null; unit_amount: number; flat_amount: number }
export interface CurrencyOption { unit_amount: number; tiers?: Tier[] | null }
export interface Product {
  id: string; key: string; name: string; description: string | null; kind: ProductKind;
  plan_key: string | null; addon_key: string | null; pack_key: string | null; credits: number | null;
  active: boolean; metadata: unknown; created_at: string; updated_at: string;
}
export interface ProductInput {
  name: string; description?: string | null; kind: ProductKind; plan_key?: string | null; addon_key?: string | null;
  pack_key?: string | null; credits?: number | null; active: boolean; metadata?: unknown;
}
export interface Price {
  id: string; key: string; product_id: string; product: string; type: PriceType; interval: BillingInterval | null;
  interval_count: number; scheme: PriceScheme; currency: string; unit_amount: number; tiers: Tier[] | null;
  currency_options: Record<string, CurrencyOption>; feature: string | null; meter_basis: MeterBasis;
  trial_days: number; tax_behavior: TaxBehavior; active: boolean; metadata: unknown; created_at: string;
}
export interface PriceInput {
  product: string; type: PriceType; interval?: BillingInterval | null; interval_count: number; scheme?: PriceScheme | null;
  currency: string; unit_amount: number; tiers?: Tier[] | null; currency_options: Record<string, CurrencyOption>;
  feature?: string | null; meter_basis?: MeterBasis | null; trial_days: number; tax_behavior?: TaxBehavior | null;
  active: boolean; metadata?: unknown;
}
export interface TaxRate { id: string; region: string; name: string; rate_bp: number; active: boolean }
export interface Coupon {
  id: string; code: string; name: string | null; kind: CouponKind; percent_bp: number | null; amount_off: number | null;
  currency: string | null; duration: CouponDuration; duration_cycles: number | null; max_redemptions: number | null;
  redeemed: number; expires_at: string | null; products: string[]; active: boolean;
}
export interface CouponInput {
  name?: string | null; kind: CouponKind; percent_bp?: number | null; amount_off?: number | null; currency?: string | null;
  duration?: CouponDuration | null; duration_cycles?: number | null; max_redemptions?: number | null;
  expires_at?: number | null; products: string[]; active: boolean;
}
export interface Customer {
  id: string; subject: string; email: string | null; name: string | null; currency: string; region: string | null;
  tax_exempt: boolean; provider: string | null; provider_customer_id: string | null; has_payment_method: boolean;
  payment_method_label: string | null; balance: number; metadata: unknown; created_at: string;
}
export interface CustomerInput {
  subject?: string; email?: string | null; name?: string | null; currency?: string | null; region?: string | null;
  provider?: string | null; tax_exempt?: boolean;
}
export interface SubscriptionItem { id: string; price: string; product: string; quantity: number }
export interface Subscription {
  id: string; customer_id: string; subject: string; status: SubscriptionStatus; currency: string; interval: BillingInterval;
  interval_count: number; cycle_anchor: string; current_period_start: string; current_period_end: string;
  trial_ends_at: string | null; cancel_at_period_end: boolean; canceled_at: string | null; ended_at: string | null;
  cancel_reason: string | null; coupon: string | null; coupon_cycles_left: number | null; pending_price: string | null;
  provider: string | null; latest_invoice_id: string | null; items: SubscriptionItem[]; version: number;
  metadata: unknown; created_at: string;
}
export interface ItemInput { price: string; quantity: number }
export interface SubscribeInput {
  subject?: string; items: ItemInput[]; coupon?: string | null; trial_days?: number | null; idempotency_key: string;
  return_url?: string | null;
}
export interface ChangePlanInput { price: string; quantity?: number | null; proration?: Proration | null; at_period_end: boolean }
export interface CancelInput { at_period_end: boolean; reason?: string | null }
export interface Order { id: string; customer_id: string; subject: string; status: OrderStatus; currency: string; invoice_id: string | null; created_at: string }
export interface OrderInput { subject?: string; items: ItemInput[]; coupon?: string | null; idempotency_key: string; return_url?: string | null }
export interface OrderPage { items: Order[]; total: number; limit: number; offset: number }
export interface PreviewLine { kind: LineKind; description: string; price: string | null; quantity: number; amount: number }
/** Server-computed proration of a plan change (no side effects). */
export interface ProrationPreview {
  subscription_id: string; currency: string; from_price: string | null; to_price: string; quantity: number; proration: Proration;
  at_period_end: boolean; effective_at: string; period_start: string; period_end: string; lines: PreviewLine[];
  credit: number; charge: number; net: number; invoiced_now: boolean; new_period_amount: number;
}
export type ServerMeterState = "ok" | "warn" | "critical" | "over";
/** One usage counter of any subject (instance-wide list). */
export interface UsageMeter {
  subject: string; feature: string; scope_key: string; used: number; reserved: number; carried_in: number; limit: number | null;
  percent: number | null; state: ServerMeterState; window_start: string; window_end: string; updated_at: string;
}
export interface UsageMeterPage { items: UsageMeter[]; total: number; limit: number; offset: number }
export interface DunningEntry {
  invoice_id: string; number: string | null; subject: string; customer_id: string; subscription_id: string | null; subscription_status?: SubscriptionStatus | null;
  currency: string; amount_due: number; attempt_count: number; retries_left: number; next_attempt_at: string | null;
  last_failure_code: string | null; last_failure_message: string | null; final_action: FinalAction; created_at: string;
}
export interface Checkout { subscription: Subscription | null; order: Order | null; invoice: Invoice | null; payment_url: string | null; replayed: boolean }
export interface InvoiceLine {
  id: string; kind: LineKind; description: string; product: string | null; price: string | null; feature: string | null;
  quantity: number; unit_amount: number | null; amount: number; discount_amount: number; tax_rate_bp: number;
  tax_amount: number; tax_inclusive: boolean; period_start: string | null; period_end: string | null;
}
export interface Invoice {
  id: string; number: string | null; customer_id: string; subject: string; subscription_id: string | null; order_id: string | null;
  status: InvoiceStatus; billing_reason: string; currency: string; subtotal: number; discount: number; tax: number;
  total: number; balance_applied: number; amount_due: number; amount_paid: number; amount_refunded: number;
  coupon: string | null; tax_region: string | null; period_start: string | null; period_end: string | null;
  attempt_count: number; next_attempt_at: string | null; hosted_url: string | null; finalized_at: string | null;
  paid_at: string | null; fulfilled_at: string | null; lines: InvoiceLine[]; created_at: string;
}
export interface Payment {
  id: string; invoice_id: string; subject: string; provider: string; provider_ref: string | null; status: PaymentStatus;
  amount: number; currency: string; amount_refunded: number; failure_code: string | null; failure_message: string | null;
  attempt: number; created_at: string; updated_at: string;
}
export interface Refund {
  id: string; payment_id: string; invoice_id: string; credit_note_id: string | null; provider: string; provider_ref: string | null;
  status: RefundStatus; amount: number; currency: string; reason: string | null; created_at: string;
}
export interface CreditNote {
  id: string; number: string; invoice_id: string; subject: string; currency: string; amount: number; refund_amount: number;
  balance_amount: number; reason: string | null; refund: Refund | null; credits_reversed: number; created_at: string;
}
export interface CreditNoteInput { amount?: number | null; to_balance: boolean; reason?: string | null; idempotency_key: string }
export interface Collection { invoice: Invoice; payment: Payment | null; payment_url: string | null }
export interface ProviderAccount { provider: string; key_id: string | null; hint: string | null; api_base: string | null; enabled: boolean; webhook_path: string; updated_at: string;
  /** Always true: only test-mode credentials are accepted. */
  test_mode: boolean;
  /** `sandbox`, `xcode` or `local_testing` for the App Store, `test` otherwise. */
  environment: string;
  /** Non-secret provider settings (never private keys or service accounts). */
  config: Record<string, unknown>;
  /** Names of the stored secrets (`api_key`, `config.private_key`, ...), never their values. */
  secrets_set: string[];
}
/** Write-only: the server seals the secrets and never returns them. */
export interface ProviderInput {
  api_key?: string | null; key_id?: string | null; webhook_secret?: string | null; api_base?: string | null; enabled: boolean;
  /** Provider-specific settings (FB-72), sealed with the secrets. Blank values keep the stored one. */
  config?: Record<string, unknown> | null;
  /** Fields to remove: `api_key`, `webhook_secret`, `key_id`, `api_base` or `config.<name>`. Omitted or blank fields keep their stored value. */
  clear?: string[];
}
export type StoreProviderId = "app_store" | "google_play" | "creem";
export const STORE_PROVIDERS: StoreProviderId[] = ["app_store", "google_play", "creem"];
/** A store product sold as a billing price. */
export interface StoreProduct { provider: string; product_id: string; price: string; kind: string; updated_at: string }
export type StorePurchaseStatus = "active" | "trialing" | "grace" | "on_hold" | "paused" | "expired" | "refunded" | "revoked" | "pending" | "replaced";
/** A store purchase bound to one subject. */
export interface StorePurchase {
  id: string; provider: string; purchase_id: string; subject: string; status: string; kind: string; price: string; product_id: string; environment: string;
  auto_renew: boolean; expires_at?: string | null; last_transaction_id?: string | null; order_id?: string | null; subscription_id?: string | null; created_at: string; updated_at: string;
}
export interface WebhookEvent { id: string; provider: string; event_id: string; event_type: string; event_at: string; received_at: string; status: string; result: string | null; attempts: number }
export interface BillingSettings {
  default_currency: string; retry_days: number[]; final_action: FinalAction; invoice_prefix: string; credit_note_prefix: string;
  incomplete_days: number; webhook_max_age_secs: number; webhook_tolerance_secs: number;
}
export interface MaintenanceReport { usage_events_read: number; usage_recorded: number; trials_ended: number; renewed: number; ended: number; retries: number; fulfilled: number; synced: number }
export interface RecurringRevenue { currency: string; mrr: number; arr: number; subscriptions: number }
export interface RevenuePoint { start: string; currency: string; gross: number; refunded: number; net: number; payments: number }
export interface BillingStats {
  from: string; to: string; recurring: RecurringRevenue[]; by_status: Record<string, number>; active_at_start: number;
  new_subscriptions: number; churned: number; churn_rate_bp: number | null; revenue: RevenuePoint[]; failed_payments: number;
}

// Entitlements (catalog and usage meters).
export type FeatureKind = "boolean" | "metered" | "gauge" | "concurrency" | "rate";
export type Period = "none" | "hour" | "day" | "month" | "lifetime" | string;
export interface Feature { key: string; name: string; description: string; kind: FeatureKind; unit: string; permissions: string[] }
export interface PlanFeature { feature: string; enabled: boolean; limit: number | null; unit: string; period: Period | null; window_secs: number | null; trial_limit: number | null }
export interface Plan { key: string; name: string; description: string; is_default: boolean; active: boolean; trial_days: number; features: PlanFeature[] }
export interface AddonFeature { feature: string; limit: number | null; period: Period | null; window_secs: number | null }
export interface Addon { key: string; name: string; description: string; active: boolean; features: AddonFeature[] }
export interface UsagePack { key: string; name: string; feature: string; units: number; valid_days: number | null; active: boolean }
export interface UsageSummary {
  feature: string; kind: FeatureKind; scope_key: string; enabled: boolean; limit: number | null; carried_in: number; used: number;
  reserved: number; remaining: number | null; pack_remaining: number; percent: number | null; enforcement: "hard" | "soft";
  period: Period; window_start: string; window_end: string | null;
}
export interface UsageAlert { id: string; at: string; subject: string; feature: string; scope_key: string; window_start: string; threshold: number; used: number; limit: number; delivered_at: string | null }

// Credits.
export type EntryKind = "top_up" | "grant" | "consumption" | "spend" | "refund" | "reversal" | "expiry";
export const ENTRY_KINDS: EntryKind[] = ["top_up", "grant", "consumption", "spend", "refund", "reversal", "expiry"];
export interface LedgerEntry {
  seq: number; id: string; at: string; subject: string; kind: EntryKind; amount: number; balance_after: number; lot_id: string | null;
  ref_entry: string | null; feature: string | null; units: number | null; usage_event_id: string | null; usage_seq: number | null;
  expires_at: string | null; idempotency_key: string; actor: string; reason: string | null; external_ref: string | null;
  request_id: string | null; metadata: unknown;
}
export interface LedgerPage { items: LedgerEntry[]; next: number | null }
export interface CreditBalance { subject: string; balance: number; expiring: { expires_at: string; amount: number }[]; updated_at: string | null }
export interface CreditLot { id: string; subject: string; entry_id: string; kind: EntryKind; amount: number; remaining: number; expires_at: string | null; created_at: string }
export interface CreditInput { subject: string; amount: number; expires_at?: number | null; idempotency_key: string; reason?: string | null }
export interface Posted { entry: LedgerEntry; replayed: boolean }
export interface CreditRate { feature: string; credits_per_unit: number; per_units: number; basis: "units" | "overage"; created_at: string; updated_at: string }
export interface ConsumptionRow { start: string; feature: string; debited: number; refunded: number; net: number; units: number; entries: number }
export interface ConsumptionStats { subject: string | null; interval: string; from: string; to: string; rows: ConsumptionRow[]; total_net: number }
export interface CreditSummary {
  subject: string | null; from: string; to: string; top_up: number; grant: number; consumption: number; spend: number; refund: number;
  reversal: number; expiry: number; net: number; balance: number; shortfall: number;
}
export interface BalancePoint { start: string; closing: number; credited: number; debited: number }
export interface BalanceHistory { subject: string; interval: string; from: string; to: string; opening: number; points: BalancePoint[] }
export interface AuditReport { subject: string; balance: number; ledger_sum: number; lots_remaining: number; last_balance_after: number; lots_match_allocations: boolean; entries_match_allocations: boolean; consistent: boolean }

export const BILLING_OPERATIONS = [
  "get /api/v1/billing/settings", "put /api/v1/billing/settings", "get /api/v1/billing/providers",
  "put /api/v1/billing/providers/{provider}", "delete /api/v1/billing/providers/{provider}",
  "get /api/v1/billing/products", "put /api/v1/billing/products/{key}", "get /api/v1/billing/prices", "put /api/v1/billing/prices/{key}",
  "get /api/v1/billing/tax-rates", "put /api/v1/billing/tax-rates/{region}", "get /api/v1/billing/coupons", "put /api/v1/billing/coupons/{code}",
  "get /api/v1/billing/customers", "put /api/v1/billing/customer", "get /api/v1/billing/subscriptions", "post /api/v1/billing/subscriptions",
  "post /api/v1/billing/subscriptions/{id}/change", "post /api/v1/billing/subscriptions/{id}/change/preview", "post /api/v1/billing/subscriptions/{id}/cancel",
  "post /api/v1/billing/subscriptions/{id}/pause", "post /api/v1/billing/subscriptions/{id}/resume",
  "get /api/v1/billing/orders", "post /api/v1/billing/orders", "get /api/v1/billing/admin/orders", "get /api/v1/billing/admin/usage-meters",
  "get /api/v1/billing/dunning", "post /api/v1/billing/products/{key}/archive", "post /api/v1/billing/prices/{key}/archive",
  "post /api/v1/billing/coupons/{code}/archive", "post /api/v1/billing/tax-rates/{region}/archive", "get /api/v1/billing/invoices", "get /api/v1/billing/invoices/{id}",
  "post /api/v1/billing/invoices/{id}/pay", "post /api/v1/billing/invoices/{id}/mark-paid", "post /api/v1/billing/invoices/{id}/void",
  "post /api/v1/billing/invoices/{id}/credit-notes", "get /api/v1/billing/credit-notes", "get /api/v1/billing/payments",
  "get /api/v1/billing/refunds", "get /api/v1/billing/webhook-events", "get /api/v1/billing/stats", "post /api/v1/billing/maintenance",
] as const;

/** Billing is off by default: its routes then answer 404. */
export const isBillingDisabled = (e: unknown) => e instanceof ApiError && e.status === 404;

function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}
const list = async <T>(path: string, signal?: AbortSignal) => (await request<{ items: T[] }>(path, { signal })).items;

// Settings and providers.
export const getBillingSettings = (signal?: AbortSignal) => request<BillingSettings>(`${B}/settings`, { signal });
export const putBillingSettings = (s: BillingSettings) => send<BillingSettings>("PUT", `${B}/settings`, s);
export const listProviders = (signal?: AbortSignal) => list<ProviderAccount>(`${B}/providers`, signal);
export const putProvider = (provider: string, input: ProviderInput) => send<ProviderAccount>("PUT", `${B}/providers/${enc(provider)}`, input);
export const deleteProvider = (provider: string) => send<void>("DELETE", `${B}/providers/${enc(provider)}`);
// Merchant-of-record stores (FB-72).
export const listStoreProducts = (signal?: AbortSignal) => list<StoreProduct>(`${B}/store-products`, signal);
export const putStoreProduct = (provider: string, productId: string, price: string) => send<StoreProduct>("PUT", `${B}/store-products/${enc(provider)}/${enc(productId)}`, { price });
export const deleteStoreProduct = (provider: string, productId: string) => send<void>("DELETE", `${B}/store-products/${enc(provider)}/${enc(productId)}`);
export const listStorePurchases = (subject?: string, signal?: AbortSignal) => list<StorePurchase>(`${B}/store-purchases${qs({ subject })}`, signal);
export const runBillingMaintenance = () => send<MaintenanceReport>("POST", `${B}/maintenance`);
export const listWebhookEvents = (limit = 100, signal?: AbortSignal) => list<WebhookEvent>(`${B}/webhook-events${qs({ limit })}`, signal);

// Catalog.
export const listProducts = (signal?: AbortSignal) => list<Product>(`${B}/products`, signal);
export const putProduct = (key: string, input: ProductInput) => send<Product>("PUT", `${B}/products/${enc(key)}`, input);
export const listPrices = (product?: string, signal?: AbortSignal) => list<Price>(`${B}/prices${qs({ product })}`, signal);
export const putPrice = (key: string, input: PriceInput) => send<Price>("PUT", `${B}/prices/${enc(key)}`, input);
export const listTaxRates = (signal?: AbortSignal) => list<TaxRate>(`${B}/tax-rates`, signal);
export const putTaxRate = (region: string, input: { name: string; rate_bp: number; active: boolean }) => send<TaxRate>("PUT", `${B}/tax-rates/${enc(region)}`, input);
export const listCoupons = (signal?: AbortSignal) => list<Coupon>(`${B}/coupons`, signal);
export const putCoupon = (code: string, input: CouponInput) => send<Coupon>("PUT", `${B}/coupons/${enc(code)}`, input);
export const archiveProduct = (key: string) => send<Product>("POST", `${B}/products/${enc(key)}/archive`);
export const archivePrice = (key: string) => send<Price>("POST", `${B}/prices/${enc(key)}/archive`);
export const archiveCoupon = (code: string) => send<Coupon>("POST", `${B}/coupons/${enc(code)}/archive`);
export const archiveTaxRate = (region: string) => send<TaxRate>("POST", `${B}/tax-rates/${enc(region)}/archive`);
export const listFeatures = (signal?: AbortSignal) => list<Feature>(`${V1}/features`, signal);
export const listPlans = (signal?: AbortSignal) => list<Plan>(`${V1}/plans`, signal);
export const listAddons = (signal?: AbortSignal) => list<Addon>(`${V1}/addons`, signal);
export const listUsagePacks = (signal?: AbortSignal) => list<UsagePack>(`${V1}/usage-packs`, signal);

// Customers, subscriptions, orders.
export const listCustomers = (limit = 200, signal?: AbortSignal) => list<Customer>(`${B}/customers${qs({ limit })}`, signal);
export const getCustomer = (subject: string, signal?: AbortSignal) => request<Customer>(`${B}/customer${qs({ subject })}`, { signal });
export const putCustomer = (input: CustomerInput) => send<Customer>("PUT", `${B}/customer`, input);
export const listSubscriptions = (subject?: string, signal?: AbortSignal) => list<Subscription>(`${B}/subscriptions${qs({ subject })}`, signal);
export const subscribe = (input: SubscribeInput) => send<Checkout>("POST", `${B}/subscriptions`, input);
export const changePlan = (id: string, input: ChangePlanInput) => send<Checkout>("POST", `${B}/subscriptions/${enc(id)}/change`, input);
export const cancelSubscription = (id: string, input: CancelInput) => send<Checkout>("POST", `${B}/subscriptions/${enc(id)}/cancel`, input);
export const pauseSubscription = (id: string) => send<Subscription>("POST", `${B}/subscriptions/${enc(id)}/pause`);
export const resumeSubscription = (id: string) => send<Checkout>("POST", `${B}/subscriptions/${enc(id)}/resume`);
export const listOrders = (subject?: string, signal?: AbortSignal) => list<Order>(`${B}/orders${qs({ subject })}`, signal);
export const listAllOrders = (f: { subject?: string; status?: OrderStatus; limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  request<OrderPage>(`${B}/admin/orders${qs(f)}`, { signal });
export const previewPlanChange = (id: string, input: ChangePlanInput, signal?: AbortSignal) =>
  request<ProrationPreview>(`${B}/subscriptions/${enc(id)}/change/preview`, { method: "POST", body: input, signal });
export const createOrder = (input: OrderInput) => send<Checkout>("POST", `${B}/orders`, input);

// Invoices, payments.
export const listInvoices = (f: { subject?: string; status?: InvoiceStatus; limit?: number } = {}, signal?: AbortSignal) => list<Invoice>(`${B}/invoices${qs(f)}`, signal);
export const getInvoice = (id: string, signal?: AbortSignal) => request<Invoice>(`${B}/invoices/${enc(id)}`, { signal });
export const payInvoice = (id: string, returnUrl?: string) => send<Collection>("POST", `${B}/invoices/${enc(id)}/pay`, returnUrl ? { return_url: returnUrl } : {});
export const markInvoicePaid = (id: string, reference?: string) => send<Invoice>("POST", `${B}/invoices/${enc(id)}/mark-paid`, reference ? { reference } : {});
export const voidInvoice = (id: string) => send<Invoice>("POST", `${B}/invoices/${enc(id)}/void`);
export const creditInvoice = (id: string, input: CreditNoteInput) => send<CreditNote>("POST", `${B}/invoices/${enc(id)}/credit-notes`, input);
export const listCreditNotes = (subject?: string, limit = 200, signal?: AbortSignal) => list<CreditNote>(`${B}/credit-notes${qs({ subject, limit })}`, signal);
export const listPayments = (subject?: string, limit = 200, signal?: AbortSignal) => list<Payment>(`${B}/payments${qs({ subject, limit })}`, signal);
export const listRefunds = (subject?: string, limit = 200, signal?: AbortSignal) => list<Refund>(`${B}/refunds${qs({ subject, limit })}`, signal);
export const billingStats = (f: { from?: string; to?: string; interval?: "day" | "month" } = {}, signal?: AbortSignal) => request<BillingStats>(`${B}/stats${qs(f)}`, { signal });

export const listUsageMeters = (f: { subject?: string; feature?: string; min_percent?: number; limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  request<UsageMeterPage>(`${B}/admin/usage-meters${qs(f)}`, { signal });
export const listDunning = (limit = 200, signal?: AbortSignal) => list<DunningEntry>(`${B}/dunning${qs({ limit })}`, signal);

// Usage meters (entitlements).
export const listUsage = (subject: string, signal?: AbortSignal) => list<UsageSummary>(`${V1}/usage${qs({ subject })}`, signal);
export const listUsageAlerts = (subject: string, signal?: AbortSignal) => list<UsageAlert>(`${V1}/usage/alerts${qs({ subject })}`, signal);

// Credits.
export const creditBalance = (subject: string, signal?: AbortSignal) => request<CreditBalance>(`${C}/balance${qs({ subject })}`, { signal });
export const creditLots = (subject: string, all = false, signal?: AbortSignal) => list<CreditLot>(`${C}/lots${qs({ subject, all })}`, signal);
export const creditLedger = (f: { subject?: string; all?: boolean; kind?: EntryKind; after?: number; limit?: number } = {}, signal?: AbortSignal) =>
  request<LedgerPage>(`${C}/ledger${qs(f)}`, { signal });
export const creditAudit = (subject: string, signal?: AbortSignal) => request<AuditReport>(`${C}/audit${qs({ subject })}`, { signal });
export const creditTopUp = (input: CreditInput) => send<Posted>("POST", `${C}/top-ups`, input);
export const creditGrant = (input: CreditInput) => send<Posted>("POST", `${C}/grants`, input);
export const listCreditRates = (signal?: AbortSignal) => list<CreditRate>(`${C}/rates`, signal);
export const creditSummary = (f: { subject?: string; all?: boolean; from?: string; to?: string } = {}, signal?: AbortSignal) => request<CreditSummary>(`${C}/stats/summary${qs(f)}`, { signal });
export const creditConsumption = (f: { subject?: string; all?: boolean; from?: string; to?: string; interval?: string } = {}, signal?: AbortSignal) =>
  request<ConsumptionStats>(`${C}/stats/consumption${qs(f)}`, { signal });
export const creditBalanceHistory = (subject: string, f: { from?: string; to?: string; interval?: string } = {}, signal?: AbortSignal) =>
  request<BalanceHistory>(`${C}/stats/balance${qs({ subject, ...f })}`, { signal });
