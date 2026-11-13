# ADR-0032: Inbound billing webhook inspector

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-billing` (`inspector.rs`, the webhook and store notification paths, REST),
  migration `0053_billing_webhook_deliveries` (SQLite and PostgreSQL), `crates/forgebase-core`
  (`[billing] webhook_log_retention_days`), `crates/forgebase-observability` (metric names),
  `crates/forgebase-server` (wiring), `crates/forgebase-db` (placement registry entry).
- Builds on: ADR-0021 (billing, provider webhooks, the event dedup table), ADR-0007 (the
  `billing.maintenance` job), ADR-0028 (both backends), ADR-0029 (placement classes).

## Context

`POST /api/v1/billing/webhooks/{provider}` verifies a delivery (constant-time HMAC for Stripe, Razorpay and
Creem, platform-certificate signatures for WeChat Pay, the JWS chain to Apple's root for the App Store, the
Google-signed push token for Google Play), deduplicates it by `(provider, event id)` in
`_forgebase_billing_webhook_events` and applies it (ADR-0021 §6, §9). That table only ever holds **verified**
events, one row per event, with a status and a short result. An operator could not see:

- a delivery that was refused (a misconfigured webhook secret, a clock skew, a wrong audience), because a
  refused delivery leaves no row at all;
- what a delivery carried (headers, body) or which billing objects it touched;
- how long processing took, or how many deliveries one event took;

and could not replay a delivery or rescue the deliveries refused while a secret was wrong, short of asking the
provider to resend.

## Decisions

### 1. A delivery log next to the event dedup table

`_forgebase_billing_webhook_deliveries` (migration `0053`) has one row per **request**, verified or not. The
event table stays what it is: the per-event idempotency record. A verified delivery links to its event row
(`webhook_event_id`); many deliveries (redeliveries, replays) can point at one event. A separate table is
needed because a refused delivery has no trustworthy event id and must not touch the dedup table.

Every request is recorded by `BillingService::receive_webhook` (the HTTP handler; `handle_webhook` is the same
without a client address), **after** it was answered internally and without changing the answer: a recording
error is logged at `warn` and the provider gets exactly the response it got before this ADR. Recording is a
separate short write transaction after processing.

| Column | Content |
|---|---|
| `provider`, `account` | The path segment (known names as is; anything else cut to 64 characters of `[A-Za-z0-9_.-]`). The account is a non-secret identifier the driver knows: Razorpay key id, WeChat Pay merchant id, App Store bundle id, Google Play package name |
| `received_at`, `client_addr` | Service clock; the socket peer, or the last `X-Forwarded-For` entry when `[auth] trust_forwarded_for` is on (the same rule as the rest of the server) |
| `headers` | JSON object of an **allowlist** only: `content-type`, `content-length`, `content-encoding`, `user-agent`, `from`, `x-request-id`, `request-id`, `forwarded`, `x-forwarded-for`, `x-forwarded-proto`, `x-real-ip`, and the provider signature headers (`stripe-signature`, `x-razorpay-signature`, `x-razorpay-event-id`, `wechatpay-signature`, `wechatpay-timestamp`, `wechatpay-nonce`, `wechatpay-serial`, `wechatpay-signature-type`, `creem-signature`). Values cut to 512 bytes. `authorization` (Google Play's push token), cookies and API keys are never stored |
| `body`, `body_size`, `body_sha256`, `body_truncated` | Raw bytes up to `body_max_bytes` (256 KiB, the route's request limit); the size and SHA-256 of the whole body (the event row's `payload_hash`) |
| `verification`, `reason` | `verified`, `signature_invalid`, `timestamp_out_of_tolerance`, `unknown_provider`, `malformed`, `rejected`, `error` (§2) and a safe reason |
| `event_id`, `event_type`, `processing`, `detail` | For verified deliveries: `applied`, `ignored`, `duplicate`, `failed`, and the processing detail (`invoice_paid`, `stale`, `too_old`, …) or the error |
| `outcome` | `processing` when verified, else `verification` (one filterable column) |
| `webhook_event_id`, `customer_id`, `subscription_id`, `order_id`, `invoice_id`, `payment_id`, `refund_id`, `store_purchase_id` | What the event touched, as far as processing knows it |
| `latency_us` | Verification plus processing, wall clock |
| `replays`, `reverify_attempts`, `reverified_at`, `claim`, `claimed_at` | Operator actions (§5, §6) |

The signature headers are kept on purpose: each one authenticates one stored body under the provider's secret,
does not reveal the secret, and is what a later re-verification checks. They are not a credential that opens
anything.

### 2. Outcomes and safe reasons

Verification errors map to outcomes by their stable code: `invalid_signature` → `signature_invalid`,
`stale_signature` (Stripe and WeChat Pay timestamps, an expired Google push token) →
`timestamp_out_of_tolerance`, `invalid_payload` → `malformed`, an unconfigured or unknown provider →
`unknown_provider`, any other refusal (another app or merchant, live mode, a bad certificate) → `rejected`, and
a verification that could not complete (the store API or the database failed) → `error`. The reason is
`<code>: <message>` for refusals, whose messages are fixed strings in the drivers ("the webhook signature does
not match"); it is only the code for `provider_error`, `internal_error` and database errors, whose text could
echo upstream content. Neither a secret nor an expected signature is ever part of a reason (tested).

A verified delivery of an event that was processed or ignored before is `duplicate`; it links to the event row.

### 3. Abuse safety: a budget for unverified deliveries

A verified delivery is authenticated by the provider and always recorded. An unverified one is attacker
controlled, so it is recorded only within a budget:

| Limit | Default | Over it |
|---|---|---|
| Unverified deliveries per provider and minute | 60 | nothing is stored |
| Stored bytes (body + headers) of unverified deliveries per provider and minute | 1 MiB | a row without body and headers |
| Unverified rows in all | 10 000 | the oldest unverified rows are evicted first |
| Bytes of unverified rows in all | 64 MiB | the oldest unverified rows are evicted first |

Unknown provider names share one budget (`other`), so path spraying gets no extra room. The per-minute windows
live in memory (one server process owns a data directory); the totals are read from the table in the insert
transaction (`verification IN (...)`, indexed). Disk use by forged traffic is therefore bounded by the total
caps, and its growth rate by the per-minute budget. The provider's HTTP answer is the same whether or not the
delivery was stored, and an unverified payload is never processed.

`[billing] webhook_log_retention_days` (default 30, `FORGEBASE_BILLING_WEBHOOK_LOG_RETENTION_DAYS`) bounds every
row: the `billing.maintenance` job deletes older rows in batches of 500 (at most 10 000 a run) and reports
`webhook_deliveries_pruned`. The event dedup table is not pruned, so a redelivery after the log expired is
still a duplicate. The budget values are `WebhookLogConfig` fields with the defaults above; only the retention
is a configuration key for now.

### 4. The event dedup moves into the write transaction as well

Two deliveries of one event that both pass the dedup read (a provider retry racing an operator
re-verification) could both apply it. The payment-provider path now reads the event row again inside the write
transaction and, if it is processed or ignored, rolls back and answers `duplicate`. Writes are serialised on
both backends (one writer), so an event is applied once. Behaviour seen by providers is unchanged.

### 5. Replay

`POST /billing/webhook-deliveries/{id}/replay` takes a **verified** delivery whose whole body is stored and runs
it through the normal processing path:

- The stored body is **verified again** with the current secrets, against the time it was received (Stripe's
  and WeChat Pay's timestamp tolerance, certificate validity and the replay window `webhook_max_age_secs` are
  checked as of `received_at`). A stored row therefore cannot inject a payload: a body that does not verify is
  reported `unverified` and not processed (for example after a secret rotation). Google Play pushes are the
  exception: their push token is short-lived and never stored, so the driver re-reads the stored push
  (`StoreProvider::reread_notification`) and, as always, reads the purchase state from the Play Developer API.
- The event dedup decides: an event that was processed is a reported no-op (`already_applied`), one that was
  ignored is `already_ignored`, a failed one is processed again (`applied`, `ignored` or `failed`).
- The replay is not a delivery: it does not count as an attempt on the event row. It updates the delivery's
  processing columns and links, increments `replays`, and writes `webhook_delivery.replay` to
  `_forgebase_billing_audit`.

`409 delivery_not_verified` for an unverified delivery, `409 body_not_stored` when the body was dropped by the
budget or cut.

### 6. Re-verify, exactly once

`POST /billing/webhook-deliveries/{id}/reverify` takes an **unverified** delivery (typically refused while a
webhook secret was misconfigured), verifies it again with the current secrets as of `received_at`, and processes
it if it now verifies. Exactly once, even under concurrent calls:

1. operator actions (replay, re-verify) run one at a time in the process (an async mutex);
2. the row is claimed with a conditional `UPDATE … WHERE verification IN (unverified…) AND (claim IS NULL OR
   claimed_at < now − 5 min)` before processing (a claim abandoned by a crash expires after five minutes);
3. the event dedup runs inside the write transaction (§4).

A delivery that now verifies becomes `verified` (with `reverified_at`) and gets its processing outcome; when
several stored deliveries carry the same event, the first one re-verified applies it and the others are
`already_applied`. One that still does not verify keeps its row with the new outcome and reason (`result:
unverified`). Every attempt that runs is audited (`webhook_delivery.reverify`). Answers: `409
delivery_already_verified` (use replay), `409 reverify_in_progress`, `409 body_not_stored`, and `409
not_reverifiable` for Google Play, whose authentication (the push token) is not stored; Pub/Sub redelivers
unacknowledged pushes by itself.

### 7. REST, authorization, OpenAPI

Under `/api/v1/billing`, tag `billing-webhooks`, platform administrators only (`require_admin`, checked before
any query parameter is parsed; a signed-in non-administrator gets `403`, an anonymous caller `401`), like the
existing `webhook-events`:

| Route | operationId |
|---|---|
| `GET /billing/webhook-deliveries?provider=&outcome=&event_type=&event_id=&from=&to=&limit=&offset=` | `billing_webhook_deliveries` |
| `GET /billing/webhook-deliveries/{id}` (headers and body; text when UTF-8, else base64) | `billing_webhook_delivery` |
| `POST /billing/webhook-deliveries/{id}/replay` | `billing_replay_webhook_delivery` |
| `POST /billing/webhook-deliveries/{id}/reverify` | `billing_reverify_webhook_delivery` |

The list is newest first with `total`, `limit` (1–500, default 50) and `offset`, like `billing/admin/orders`;
`outcome` must be one of the ten outcome names (`400 invalid_parameter`). The list never carries bodies.

### 8. Metrics

Names in `forgebase_observability::names`; labels from fixed lists (unknown providers are `other`):

| Metric | Type | Labels |
|---|---|---|
| `forgebase_billing_webhook_deliveries_total` | counter | `provider`, `outcome` (every delivery, stored or not) |
| `forgebase_billing_webhook_processing_seconds` | histogram (1 ms … 10 s) | `provider` (deliveries, replays and re-verifications) |
| `forgebase_billing_webhook_log_dropped_total` | counter | `provider`, `kind` = `body` (row without body) or `delivery` (no row) |

### 9. Storage

Migration `0053_billing_webhook_deliveries` is a SQLite/PostgreSQL pair (`bytea` body, `smallint` flag, row-level
security enabled on PostgreSQL like every table after 0040), indexed by `received_at`, `(provider,
received_at)`, `(outcome, received_at)`, `(verification, received_at)` and `(provider, event_id)`. Placement
class `control`, like every billing table. Forward-only; rollback is the pre-migration snapshot (SQLite) or a
backup (PostgreSQL), as for every migration.

## Consequences

- Operators see every delivery, including refused ones, and can rescue deliveries refused by a configuration
  mistake without the provider's help, at most once.
- **Bodies hold personal data** (customer names, emails, addresses, partial card descriptions, store
  receipts): they are readable by platform administrators only, kept for the retention period, and are part
  of backups. `docs/security.md` lists the controls.
- One more short write per webhook delivery (and an aggregate read per stored unverified one). Webhook traffic
  is small next to the API; the budget bounds the forged share.
- Limits: the per-minute budget is per process and resets on restart; a body over 256 KiB (only possible with a
  lower configured cap) cannot be replayed; Google Play deliveries cannot be re-verified; an ignored event
  cannot be forced through again (the dedup is authoritative, as for provider redeliveries); store
  notifications are applied outside one transaction (ADR-0021 §9), so a live store redelivery racing an
  operator re-verification relies on the store's idempotency (unique transaction ids) rather than the
  in-transaction dedup; rows received in the same millisecond list in id order. The budget values other than
  the retention are not configuration keys yet.
- Follow-ups for other areas: dashboard screens (a deliveries table with filters, a detail view with headers
  and body, replay and re-verify buttons), SDK wrappers for the four operations, and alerting on
  `forgebase_billing_webhook_deliveries_total{outcome="signature_invalid"}`.
