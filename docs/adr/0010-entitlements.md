# ADR-0010: Entitlements and quotas

- Status: accepted
- Date: 2026-11-09
- Work package: FB-24 (MDF-44). Migration `0009_entitlements.sql`. Crate `forgebase-entitlements`.
- Scope: docs/requirements/authz-entitlements-billing.md §B, §C and §F (FB-24 rows): plans, features,
  add-ons, usage packs, trials, user and org entitlements, overrides, enterprise limits, quota policies,
  immutable usage events, counters, atomic reservation, leases, alerts, rollover, plan-based API rate
  limits, the FB-21 pipeline hooks, the billing sync seam.
- Builds on: ADR-0008 §2 (subjects) and §8 (`EntitlementHook`, `QuotaHook`, `HookRequest`,
  `PolicyService::set_hooks`), ADR-0007 (jobs, `ApiExtension`), ADR-0001 (one writer, error model).
- Leaves to later cards: payments, invoices, prices and the billing UI (FB-70/71), credits (FB-73),
  storage metering calls (FB-30), SDK helpers (FB-41).

## 1. Crate and changes elsewhere

`forgebase-entitlements` is its own crate (like `forgebase-policy`): it owns tables, a REST surface, job
handlers and an HTTP gate, and depends on `forgebase-policy` (hooks, permission checks, pattern
matching), `forgebase-jobs` (alert delivery, the maintenance schedule) and `forgebase-api` (extension
and gate types). Nothing below it depends on it.

| Crate | Change | Compatibility |
|---|---|---|
| `forgebase-api` | `gate` module: `RequestGate` trait, `GateDecision`; `RouterOptions::gates` (run after authentication and tenancy, before the handler) | additive; every `RouterOptions` literal uses `..Default` |
| `forgebase-migrations` | migration 9 `entitlements` (listed between 7 and 12; the migrator applies gaps) | append-only |
| `forgebase-server` | opens the service after the policy service, `policy.set_hooks(ent.hooks())`, registers the job handlers, creates the schedule, mounts the extension and the rate gate; `RunningServer::entitlements()`; `ServerError::Entitlements` | additive |

## 2. Model (migration 0009)

| Table | Holds |
|---|---|
| `_forgebase_features` | key, kind (`boolean`, `metered`, `gauge`, `concurrency`, `rate`), unit, gated permission patterns |
| `_forgebase_plans`, `_forgebase_plan_features` | plans (one default plan at most), per feature: enabled, limit (`NULL` = unlimited), unit, period (`hour`, `day`, `month`, `lifetime`, `rolling` + `window_secs`, `none`), trial limit |
| `_forgebase_addons`, `_forgebase_addon_features` | add-ons: enable a feature and/or add units per quantity |
| `_forgebase_usage_packs`, `_forgebase_pack_grants` | one-off extra units (catalog; what a subject holds, `remaining`, expiry) |
| `_forgebase_quota_policies` | per feature: scope (`subject`, `user`, `team`, `resource`), `hard`/`soft` (+ `overage_pct` ceiling), rollover (`rollover_pct`, `rollover_cap`), `cost`, lease length, permissions admitted at step 6 |
| `_forgebase_subscriptions`, `_forgebase_subscription_addons`, `_forgebase_billing_sync_events` | a subject's plan, status, trial end, billing period, cycle anchor, cancellation, add-ons; applied billing events |
| `_forgebase_entitlement_overrides` | allow/deny exceptions for a subject, a member inside a subject, or a user everywhere; optional limit and expiry |
| `_forgebase_custom_limits` | enterprise contract grants per subject and feature |
| `_forgebase_usage_counters` | per (feature, subject, scope key, window): used, reserved, carried in, limit in force, alert level |
| `_forgebase_usage_reservations`, `_forgebase_quota_leases` | holds (`held` → `committed`/`released`/`expired`) and concurrency leases |
| `_forgebase_usage_events` | **immutable** usage records (triggers refuse UPDATE and DELETE); `seq` is FB-70's cursor; idempotency key unique per subject |
| `_forgebase_usage_alerts`, `_forgebase_entitlement_audit` | alerts at 70/90/100 %; append-only audit of catalog/subject changes, alert deliveries, resets |

Times are epoch milliseconds in storage and RFC 3339 on the wire. Subjects are `org:<id>` or a
principal key (`user:<collection>/<id>`, `service:<id>`), per ADR-0008 §2: the organization a check acts
in, else the caller. The catalog is instance-wide (not per realm); FB-27/FB-70 may scope it later.

## 3. Resolution: entitlement is not quota

The catalog and subject tables load into one immutable snapshot, swapped after every write (the
ADR-0008 §7 pattern); per (subject, member) results are cached until the next time-dependent change
(trial end, cancellation, override expiry). Order for subject `S` and member `U`, following the
requirements' priority list:

1. *(platform super-admin: the pipeline skips the hooks, ADR-0008 §8)*
2. plan of `S`'s subscription when it entitles — `active`, `past_due`, or `trialing` before
   `trial_ends_at`, and not past `cancel_at` — else the default plan; while trialing, `trial_limit`
   replaces the limit (the trial allowance). **In an organization, the organization's plan decides
   (priority 5); the user's personal plan (priority 6) applies in their personal scope.**
3. add-ons of `S`: enable features, add `limit × quantity`;
4. enterprise custom limits of `S`: replace the grant;
5. allow overrides, least specific first — `U` everywhere, then `S`, then `U` inside `S` (priority 7,
   user quota overrides); among equal specificity the newest applies;
6. deny overrides from any of those: always win (priority 2, explicit deny).

Quota availability (priority 8) is then checked against the resulting limit. `has({feature})` reads
the grant; `has({permission})` is the FB-21 permission **and** every feature gating it; `has({plan})`
compares the plan in force.

## 4. Pipeline hooks (ADR-0008 §8)

Both answer from memory (snapshot and usage cache), no I/O.

- **Step 4, `EntitlementGate`**: a permission is gated by every feature whose `permissions` patterns
  match it, and `org:<feature>:<permission>` keys by the catalog feature `<feature>` (Clerk's "custom
  organization permissions are tied to features"). Any gating feature not enabled → deny. Ungated
  permissions pass, so an empty catalog changes nothing.
- **Step 6, `QuotaGate`**: for every quota policy whose `permissions` match, admission is denied when a
  hard quota cannot cover `cost` (window allowance + rolled-over units + usage packs), a soft quota is at
  its overage ceiling, a gauge is at its limit, or every concurrency lease is taken. The hook **admits;
  it does not consume**: the operation meters itself with the API below, inside its own flow. Counters
  are scoped by the policy (user scope: the caller; team: a `team` resource; resource: the resource ref;
  missing scope → no admission check).

The usage cache is written after each committed metering transaction with the state read inside it,
ordered by a sequence taken while holding the single writer (late publishers never overwrite newer
state), and warmed at start. A stale or missing entry admits; the metering call decides exactly.

## 5. Metering

All in one database transaction on the single writer; the counter `UPDATE` also carries the ceiling in
its `WHERE` clause, so a backend with concurrent writers cannot over-grant either.

- **consume** = reserve + commit at once; **reserve** holds units (TTL 1 s–24 h, default 5 min) →
  **commit** (all or part; the rest is released, pack units first) or **release**; expired holds are
  given back by the next operation on the key and by the maintenance job.
- **Allocation**: window allowance (limit + carried) first, then usage packs (oldest expiry first,
  recorded on the reservation so a release restores them), then — soft quotas only — overage up to the
  ceiling. Overage units are recorded on the usage event for FB-70.
- **Idempotency**: consume, reserve, gauge and lease calls require a key, unique per subject; a replay
  returns the first result (`replayed: true`, HTTP 200); the same key with other parameters is `409
  idempotency_conflict`. Denials are not stored, so a retry after a reset can succeed.
- **Periods**: fixed UTC hours and days; calendar months, or billing cycles from the subscription's
  `cycle_anchor` (the anchor's day of month and time, clamped to short months); lifetime; rolling windows
  are exact sliding sums over the events (and held reservations) of the last `window_secs`. Gauges keep
  a level (delta or value, never below zero); concurrency counts unexpired, unreleased leases.
- **Rollover** (calendar periods, `rollover_pct` > 0): a new window's counter row carries
  `min(unused of the previous window, rollover_pct % of the limit, rollover_cap)`; never compounding
  beyond that cap. The maintenance job opens every subscribed subject's current window so an idle
  window still rolls over.

## 6. Alerts, jobs, audit

A usage change that crosses 70, 90 or 100 % of the window allowance raises each crossed threshold once
per window (`alert_level` updated conditionally in the same transaction; gauges re-arm when the level
drops). After the commit an `entitlements.alert` job (idempotency key `alert:<id>`) publishes the alert
to the `AlertSink` (default: a log line; FB-40 can publish it realtime), writes an audit row and marks
it delivered (at-least-once publish, delivered once). `entitlements.maintenance` (schedule
`entitlements-maintenance`, every minute) expires holds, leases and ended trials, opens rollover windows
and re-queues alerts whose job was lost.

## 7. Plan-based API rate limits

`RateGate` (a `RequestGate`) counts `/api/v1/*` requests of non-privileged, authenticated callers per
subject in fixed UTC hour or day windows, for every `rate` feature granted with a limit; over the limit:
`429 rate_limited` with `Retry-After`; otherwise `X-RateLimit-Limit/Remaining/Reset` of the tightest
window. A request refused by one window is not counted in the others. Counters are in memory (a rate
limit, not billed usage): they restart with the process and are per process.

## 8. REST and authorization

Under `/api/v1`, documented in the OpenAPI document (tags `entitlements`, `entitlements-catalog`,
`usage`): `entitlements[/has|/subscription|/trials|/addons|/usage-packs|/overrides[/{id}]|/limits]`,
`features[/{key}]`, `plans[/{key}]`, `addons[/{key}]`, `usage-packs[/{key}]`,
`quota-policies[/{feature}]`, `quotas/check`, `usage[/consume|/reservations[/{id}/commit|/release]|
/leases[/{id}[/renew]]|/gauge|/events|/alerts|/maintenance]`.

| Operation | Who |
|---|---|
| catalog and subject writes (plans, features, add-ons, packs, policies, subscriptions, trials, overrides, enterprise limits) | platform administrators only: an org's billing manager must not raise their own limits |
| catalog reads | any authenticated caller |
| entitlements, `has()`, quota dry run | the subject itself, or active members of the org (`org:read`) |
| usage, events, alerts, subscription, packs held | the subject itself, or `billing:read` in the org |
| consume, reserve, commit, release, leases, gauges | `usage:write` (trusted backends, service accounts) or privileged; **never implied for the subject itself**, who could release its own holds |

Another subject's reservation or lease is `404`. Quota denials are `429 quota_exceeded` with the
decision in `details`; a feature outside the plan is `403 feature_not_entitled`.

## 9. Billing sync seam

`BillingSync` (implemented by `EntitlementService`) is what FB-70 calls: `sync_subscription` sets the
full subscription state (plan, status, trial and period dates, cycle anchor, cancellation, add-ons) —
idempotent by `event_id`, ignoring events older than the last applied `event_at` (`stale`) — and
`grant_usage_pack` (idempotent by key). No payment concepts here; FB-70 reads `usage_events` by `seq`.

## Consequences

- Checks cost no I/O; a catalog or subject write reloads the whole snapshot (fine for thousands of
  subscriptions; FB-81 measures; an incremental reload is a follow-up).
- The cache and rate counters are per process: like ADR-0008 §7, a second writer process is not seen
  until restart; multi-node needs a change feed (FB-40) and shared rate counters (KV).
- Rolling windows have no alerts and no rollover; the admission check of a rolling window admits when
  its cache entry is stale (the metering call is exact).
- Follow-ups: storage metering in FB-30 (`adjust_gauge`), metering of record creation through a data
  ChangeHook (FB-40 adds one), Billing Studio screens (FB-71), SDK `has()` helpers (FB-41), per-realm
  catalogs, batch metering of API requests into usage events for monthly API quotas.
- Dependencies: none new (ADR-0002).
