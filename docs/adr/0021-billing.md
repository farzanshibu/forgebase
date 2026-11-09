# ADR-0021: Billing

- Status: accepted
- Date: 2026-11-09
- Work package: FB-70 (MDF-36). Migration `0020_billing.sql`. Crate `forgebase-billing`.
- Scope: SPEC §15 and §27 scenario 20; docs/requirements/authz-entitlements-billing.md §B.7, §C (FB-70 row),
  §F (multi-currency pricing, payment-to-entitlement mapping, payment statistics): a proper billing
  application — subscriptions (monthly, yearly), one-time purchases, usage-based billing, trials, add-ons,
  usage packs, overage, proration, coupons, tax fields, invoices, credit notes and refunds, dunning, a
  customer portal, Stripe and Razorpay with signed idempotent webhooks, entitlement sync; off by default.
- Builds on: ADR-0010 (FB-24: plans, add-ons, usage packs, usage events with `seq`, `BillingSync`), ADR-0016
  (FB-73: `CreditSink`), ADR-0008 (subjects, the pipeline, `billing:read`/`billing:write`), ADR-0007 (jobs,
  `ApiExtension`), ADR-0015 (FB-25 outbound HTTPS), ADR-0001 (one writer, error model).
- Leaves to later cards: the Billing Studio screens (FB-71); SDK helpers (FB-41/FB-82).
- Amended 2026-11-10 by FB-72 (MDF-50): Creem, WeChat Pay, App Store and Google Play (§9, migration
  `0023_billing_stores.sql`).

## 1. Crate and changes elsewhere

`forgebase-billing` sits on top of `forgebase-entitlements` and `forgebase-credits`. Nothing below it depends
on it.

| Crate | Change | Compatibility |
|---|---|---|
| `forgebase-core` | `Config::billing` (`[billing] enabled = false`, `FORGEBASE_BILLING_ENABLED`) | additive; default off |
| `forgebase-migrations` | migration 20 `billing` (after 16; 17–19 belong to parallel cards, the migrator applies gaps) | append-only |
| `forgebase-server` | when enabled: opens `BillingService` (keyring of the data directory, FB-25 `HttpsClient`), attaches jobs, creates the schedule, registers the handler, mounts the extension; `RunningServer::billing()`; `ServerError::Billing` | additive |

No new third-party dependencies (hmac, sha2, subtle, base64, url are workspace crates already).

## 2. Money

Every amount is an `INTEGER` of **minor units** (cents, paise, yen) next to an ISO 4217 code; there is no
floating point anywhere. `money.rs` does the arithmetic: `mul_div` in `i128` rounding half away from zero,
amounts capped at 10¹³, proration `amount × (end − now) / (end − start)`, tax (exclusive: `net × bp /
10000` on top; inclusive: `net − net × 10000 / (10000 + bp)` inside; signed, so credit lines carry negative
tax), discount allocation that sums exactly. Currency exponents (JPY 0, KWD 3) are known for display only.

## 3. Model (migration 0020)

| Table | Holds |
|---|---|
| `_forgebase_billing_products` | what is sold and what a paid line delivers: `plan` → FB-24 plan, `addon` → FB-24 add-on, `usage_pack` → FB-24 pack, `credits` → FB-73 credits per unit, `service` → nothing automatic (**payment-to-entitlement mapping**) |
| `_forgebase_billing_prices` | `recurring` (in advance; month/year × count), `one_time`, `metered` (in arrears from a FB-24 feature; basis `units` or `overage`); scheme `per_unit`, `graduated`, `volume` with tiers; primary currency plus `currency_options` (**multi-currency price set**); trial days; tax behaviour |
| `_forgebase_billing_tax_rates` | rate in basis points per region (`IN`, `DE`, `US-CA`; the country applies to sub-regions). No tax engine |
| `_forgebase_billing_coupons` | percent (bp) or amount (currency) off; once / repeating n / forever; cap, expiry, product restriction |
| `_forgebase_billing_customers` | one per subject (`org:<id>` or a principal key: users **and** organizations); currency (locked once invoiced), region, provider customer id, a **provider reference** to the saved payment method (never card data), `balance` owed to the customer |
| `_forgebase_billing_subscriptions`, `_items` | status `incomplete`/`trialing`/`active`/`past_due`/`paused`/`canceled`, periods, cycle anchor, trial, cancel at period end, coupon cycles left, pending (period-end) price, `version` / `synced_version` |
| `_forgebase_billing_orders` | one-time purchases |
| `_forgebase_billing_invoices`, `_lines` | gapless numbers, subtotal, discount, tax, total, balance applied, due, paid, refunded, dunning attempt and next retry, hosted URL, `fulfilled_at`. Lines of a finalized invoice are immutable (triggers) |
| `_forgebase_billing_sequences` | gapless counters per prefix (`INV-000001`, `CN-000001`) |
| `_forgebase_billing_payments`, `_refunds`, `_credit_notes` | collection attempts (unique idempotency key, provider ref, newest provider event time), refunds, numbered credit notes (refund part + balance part = amount) |
| `_forgebase_billing_usage_records`, `_cursors` | one row per FB-24 usage event and metered price (idempotent), billed by an invoice; the feed cursor |
| `_forgebase_billing_providers` | provider accounts; API secret and webhook secret **sealed** with the instance keyring (AAD `billing:provider:<name>`), never returned |
| `_forgebase_billing_webhook_events` | unique per (provider, event id): status, result, attempts, payload hash |
| `_forgebase_billing_settings`, `_audit` | dunning schedule, prefixes, windows; append-only audit (triggers) |

## 4. Flows

Every write is one transaction on the single writer. **Provider HTTP never runs inside a transaction**: a
`pending` payment row (key `<invoice>:attempt:<n>`) is committed, the provider is called with that key as its
idempotency key, the result is applied in a second transaction. A transport failure leaves the payment
pending; the next attempt reuses its key, so the provider charges at most once per attempt.

- **Subscribe** (idempotent by key; one plan per subject): with a trial → `trialing`, nothing charged, synced
  to FB-24 as trialing. Otherwise the first period is invoiced and collected: saved payment method → off-session
  charge; none → the provider's hosted page (`payment_url`); the subscription is `incomplete` (not entitled)
  until paid and expires after `incomplete_days`.
- **Plan change** mid-cycle: credit `prorate(old)`, charge `prorate(new)` for the rest of the period.
  `always_invoice` (default) invoices both now — a net charge is collected; a net credit (downgrade) makes the
  invoice total negative, credits the customer balance and is applied to the next invoices. `create_prorations`
  adds the lines to the subscription's draft invoice, finalized with the renewal; `none` switches without
  proration; `at_period_end` stores a pending price applied at renewal. Trials switch without proration.
- **Renewal** (maintenance): recurring lines for the next period in advance, usage lines for the past period
  in arrears, pending prorations, coupon (cycles counted down), tax, customer balance; then collection.
  Periods are computed from the anchor (`add_months`), so the 31st stays the 31st after February.
- **Cancel** at period end (entitled until then; FB-24 gets `cancel_at`) or now (partial-period usage and
  pending prorations invoiced). **Pause** (not entitled, no invoices) and **resume** (new period from now).
- **One-time orders** (credits, usage packs, services) are invoiced and collected the same way.
- **Dunning**: a failed renewal attempt makes the subscription `past_due` (still entitled, FB-24 `past_due`)
  and schedules retries at `finalized + retry_days[k]` (default 1, 3, 5 days); after the last failure the
  invoice is `uncollectible` and the subscription `canceled` (`payment_failed`), or left past due
  (`final_action`). A renewal without a saved method counts as a failed attempt.
- **Fulfillment** after payment, idempotent by `billing:<invoice>:<line>`: credits products →
  `CreditSink::credit_purchase`, usage packs → `BillingSync::grant_usage_pack`. Plans and add-ons reach FB-24
  through the **entitlement sync**: `BillingSync::sync_subscription` with the plan product's plan, add-on items,
  status, trial, period, cancel_at, event id `billing:<sub>:v<version>` and event time = `updated_at`
  (strictly increasing). Both are retried by maintenance (`fulfilled_at IS NULL`, `synced_version < version`).
- **Credit notes and refunds** (operators): a numbered credit note for up to what was paid; refunded to the
  payment method (provider refund, key `refund:<note>`) or credited to the balance. A refund the provider
  refuses settles to the balance instead, so a credit note always settles. Credits bought on the invoice are
  taken back in proportion (`CreditSink::reverse_purchase`).
- **Manual billing**: provider `manual`; invoices stay open until an operator records the payment
  (`mark-paid`).

## 5. Metered usage

The consumer (in maintenance, before renewals) reads FB-24 usage events after its cursor in `seq` order
(batches of 200) and, for subjects with an `active` or `past_due` subscription holding a metered price of the
event's feature, stores `units` or `overage_units` as a usage record keyed by event id and price (**replaying
the feed never bills twice**); the cursor moves in the same transaction. Gauges are skipped. The renewal bills
the unbilled records before the period end through the price's scheme and marks them billed. Overage billing
is a metered price with basis `overage`; extra usage packs are one-time products.

## 6. Providers and webhooks

`PaymentProvider`: `create_customer`, `charge` (off-session), `refund`, `hosted_payment`, `hosted_setup`,
`verify_webhook`. Drivers: **Stripe** (form-encoded REST, Bearer key, `Idempotency-Key`; webhook
`Stripe-Signature: t=…,v1=…` = HMAC-SHA256 of `"<t>.<body>"`, tolerance 300 s), **Razorpay** (JSON REST, Basic
auth, orders + recurring payments, payment links, auth links; webhook `X-Razorpay-Signature` = HMAC-SHA256 of
the body, `X-Razorpay-Event-Id`), **manual**. Only **test-mode keys** are accepted (`sk_test_`/`rk_test_`,
`rzp_test_`; live keys → `400 live_mode_refused`). Tests use a mock HTTP client; nothing calls a provider.

`POST /api/v1/billing/webhooks/{provider}` (anonymous; the signature is the authentication):

1. signature checked in constant time → forged or tampered: `400 invalid_signature`; Stripe timestamp outside
   the tolerance → `400 stale_signature` (replayed delivery);
2. event id unique per provider → a processed or ignored event is answered `duplicate` and changes nothing; a
   failed one is processed again on redelivery;
3. events older than `webhook_max_age_secs` (default 3 days) → `ignored/too_old` (the replay window for
   Razorpay, whose signatures carry no timestamp);
4. **out of order**: payments and refunds keep the provider time of the newest applied event; older events →
   `ignored/stale`; a succeeded payment never goes back to failed (`already_succeeded`);
5. the state change and the event row commit in one transaction (**durable**); fulfillment and sync follow
   and are retried by maintenance.

Events: `payment_intent.succeeded`/`payment_failed`, `refund.*`, `setup_intent.succeeded` (Stripe);
`payment.captured`/`failed`, `payment_link.paid`, `order.paid`, `refund.*`, `token.confirmed` (Razorpay). A
successful payment carrying a payment method saves it for renewals.

## 7. Statistics

`GET /billing/stats` (realm-level `billing:read`): MRR per currency (active and past-due subscriptions,
recurring prices normalised to a month, after coupons) and ARR = 12 × MRR, **no currency conversion**;
subscriptions per status; active at window start, new, churned (never-paid subscriptions excluded) and churn
rate in basis points; revenue per currency and day or month (succeeded payments less succeeded refunds);
failed payments.

## 8. REST and authorization

Under `/api/v1/billing` (tags `billing`, `billing-catalog`, `billing-portal`, `billing-webhooks`):
`settings`, `providers[/{provider}]`, `products[/{key}]`, `prices[/{key}]`, `tax-rates[/{region}]`,
`coupons[/{code}]`, `customer`, `customers`, `subscriptions[/{id}[/change|/cancel|/pause|/resume]]`,
`orders[/{id}]`, `invoices[/{id}[/pay|/mark-paid|/void|/credit-notes]]`, `credit-notes`, `payments`,
`refunds`, `portal`, `portal/payment-method`, `webhooks/{provider}`, `webhook-events`, `stats`, `maintenance`.

| Operation | Who |
|---|---|
| catalog, tax rates, coupons, providers, settings, maintenance, webhook events | platform administrators |
| catalog reads | any authenticated caller |
| customer, subscribe, change plan, cancel, pause, resume, orders, pay, payment-method page (**customer portal**) | the subject itself (personal subjects), `billing:write` in the organization (owner, billing manager) or at realm level |
| reads of a subject's billing (portal, invoices, payments, credit notes) | the subject itself, `billing:read` in the organization or at realm level |
| credit notes and refunds, mark paid, void, tax exemption | `billing:write` **at realm level** (operators) or privileged: an organization must not refund itself |
| statistics, every customer | realm-level `billing:read` or privileged |
| webhooks | anyone with a valid provider signature |

Another subject's objects are `404`.

## 9. More providers (FB-72)

Two kinds of provider, one account table (`PUT /billing/providers/{p}`, secrets **and** the new
provider-specific `config` sealed together under AAD `billing:provider:<p>`), one webhook route
(`POST /billing/webhooks/{p}`) and one event table (dedup per provider + event id, as §6).

| Provider | Kind | Test mode is enforced by |
|---|---|---|
| `wechat_pay` | **payment provider** (`PaymentProvider`): collects ForgeBase invoices | WeChat Pay has no test keys: `api_base` is required and the production hosts (`api.mch.weixin.qq.com`, `api2.…`) are refused |
| `app_store` | **store** (`StoreProvider`): merchant of record, owns payment and renewals | `environment` sandbox/xcode/local_testing; production API base and `Production` transactions/notifications refused |
| `google_play` | store | purchases without `testPurchase` (subscriptions) or `purchaseType = 0` (products) refused |
| `creem` | store (merchant of record selling on the web) | `creem_test_…` keys only, `test-api.creem.io`; `mode: prod` payloads refused |

**WeChat Pay API v3.** Requests carry `Authorization: WECHATPAY2-SHA256-RSA2048 mchid,nonce_str,signature,
timestamp,serial_no` (RSA-SHA256 PKCS#1 v1.5 of `METHOD\nPATH\nTS\nNONCE\nBODY\n`, merchant key). Every 2xx
response and every notification is verified with the platform key named by `Wechatpay-Serial` over
`TS\nNONCE\nBODY\n`; `platform_keys` holds several certificates or platform public keys for rotation;
unknown serials, expired or not-yet-valid certificates, and timestamps outside the webhook tolerance are
refused. Notification resources (`AEAD_AES_256_GCM`, APIv3 key, nonce, associated data) are decrypted and
the decrypted `mchid` must be ours. Invoices are paid on a Native (QR `code_url`) payment, `attach` = invoice
id, `out_trade_no` = first 32 hex of SHA-256 of the idempotency key; refunds read the original total back
(`GET /v3/pay/transactions/id/{id}`) and call `/v3/refund/domestic/refunds`. CNY only; no off-session charge
(renewal invoices are paid on the hosted page, like a customer without a saved method).

**Stores.** The store took the money; ForgeBase verifies, server side, what the store reports and mirrors
it. `_forgebase_billing_store_products` maps a store product id to a price (recurring for subscriptions,
one-time otherwise); the product kind of the price decides what is delivered (§3). A purchase row
(`_forgebase_billing_store_purchases`, unique per store + stable purchase id) binds the purchase to **one**
subject: a second account presenting the same receipt gets `409 purchase_owned`.

| Store report | Billing |
|---|---|
| new transaction (purchase, renewal) | subscription created/moved to the store's period, a **paid** invoice (one line, no ForgeBase tax: the store is merchant of record, store price when reported else the catalog price) with a succeeded payment whose `provider_ref` is the transaction id (unique: replays add nothing), FB-24 sync; one-time products: a paid order, credits / usage packs fulfilled |
| auto-renew off / on | `cancel_at_period_end` |
| grace period | `past_due` (FB-24 keeps access) |
| billing retry on hold, paused | `paused` (no access) |
| expired | `canceled`, `cancel_reason = store_expired` |
| refund, revocation, voided purchase | credit note with an **external** refund (recorded `succeeded` with the store reference, idempotency key `store:<p>:<refund ref>`), FB-73 credits reversed in proportion, subscription ended at once (`store_refunded` / `store_revoked`) |

Out-of-order reports are ignored by store time (`last_event_at`, from `signedDate` / `eventTimeMillis` /
`created_at`), except that a transaction is always invoiced (money taken is a fact). Store-managed
subscriptions are never renewed, dunned, changed, paused or cancelled by ForgeBase (`409 store_managed`);
maintenance reads subscriptions past their period end back from the store (at most hourly per purchase), so
a missed renewal or expiry notification heals. An operator credit note on a store invoice cannot refund at
the store (stores refund themselves) and settles to the customer balance (§4).

| Store | Verification (`POST /billing/store/{p}/verify`) | Notifications | Authentication |
|---|---|---|---|
| App Store | StoreKit 2 `signed_transaction`, or `transaction_id` read with the App Store Server API (`/inApps/v1/transactions/{id}`); subscriptions also read status and renewal info (`/inApps/v1/subscriptions/{originalTransactionId}`) | Server Notifications V2 `{"signedPayload"}`, dedup by `notificationUUID`; `TEST` recorded and ignored | every JWS (payload, transaction, renewal info) verified ES256 against its `x5c` chain to the **pinned Apple Root CA - G3** (SHA-256 `63:34:3A:BF:…:91:79`) with `rustls-webpki`, plus Apple's marker extensions on leaf (`1.2.840.113635.100.6.11.1`) and intermediate (`1.2.840.113635.100.6.2.1`) so a developer certificate from the same root cannot sign; bundle id must match. Outbound: ES256 JWT of the App Store Connect key (`aud appstoreconnect-v1`, `bid`) |
| Google Play | `purchase_token` + `product_id` (+ `kind`): `purchases.subscriptionsv2.get` / `purchases.products.get`, then acknowledged | Real-time developer notifications as Pub/Sub push; dedup by Pub/Sub `messageId`; the purchase state is always read back from the API; `voidedPurchaseNotification` refunds | push OIDC token: RS256 against Google's JWKS (cached an hour, refetched for an unknown `kid`), issuer `accounts.google.com`, configured audience, push service account with `email_verified`, not expired. Outbound: service-account JWT (RS256) exchanged at the token endpoint, cached until expiry |
| Creem | `checkout_id` of the success redirect read with `GET /v1/checkouts` | `creem-signature` HMAC-SHA256 hex of the body; dedup by event `id`; `subscription.paid` invoices, `refund.created` refunds only (state comes with the subscription events) | checkouts are opened by ForgeBase (`POST /billing/store/creem/checkout`) with `metadata.forgebase_subject`, which comes back HMAC-signed and binds the purchase |

New routes (add-only; operationIds `billing_store_products`, `billing_put_store_product`,
`billing_delete_store_product`, `billing_store_verify`, `billing_store_checkout`, `billing_store_purchases`):
`GET /billing/store-products` (callers), `PUT|DELETE /billing/store-products/{provider}/{product_id}`
(administrators), `POST /billing/store/{provider}/verify` and `/checkout` (`can_manage` on the subject),
`GET /billing/store-purchases` (`can_read`; instance-wide readers may omit the subject).

Crypto: `ring` (ECDSA P-256 sign/verify, RSA PKCS#1 v1.5 SHA-256 sign/verify, AES-256-GCM),
`rustls-webpki` (X.509 chain to a pinned anchor), `rustls-pki-types`; all three were already in the tree
through `rustls` (ISC / Apache-2.0 / MIT, no new crates in `Cargo.lock`). A small DER reader
(`provider/der.rs`) reads certificate serials, validity, public keys and extension OIDs. Tests sign with
test-only keys and a test chain (`tests/fixtures/stores/gen.sh`) given to the App Store driver in place of
Apple's root; no test reaches the network.

## Consequences

- Off by default: with `[billing] enabled = false` nothing is mounted and no job runs (the migration still
  creates the empty tables).
- Prices on a subscription cannot change their amounts (`409 price_in_use`): new prices and plan changes keep
  invoices reproducible.
- One currency per customer; MRR is reported per currency. Exchange rates are out of scope.
- Usage events that arrive after their period was invoiced are billed with the next period.
- Webhook events are processed in the request; a burst is bounded by the single writer.
- `incomplete_days` (default 1) also bounds manual invoices of a first period; operators using manual billing
  raise it.
- Follow-ups: partial payments, invoice PDFs, tax engine integration, FB-40 realtime billing events.
- FB-72 limits: test environments only (live mode is a later, explicit decision, as for Stripe); App Store
  non-consumables and non-renewing subscriptions are one-time orders (no "lifetime plan" kind yet); Apple
  `REFUND_REVERSED` and Google price-change and deferral details are recorded, not acted on; WeChat Pay
  platform certificates are configured, not downloaded (`/v3/certificates`), and there is no WeChat
  contract (papay) for off-session renewals; App Store `appAccountToken` and Google obfuscated account ids
  are not trusted to bind a purchase (only a verified claim by the subject, or Creem's signed metadata).
  A store purchase can hold a plan only when the subject has no other live plan (`409 already_subscribed`).
