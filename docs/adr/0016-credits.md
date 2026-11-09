# ADR-0016: Credits

- Status: accepted
- Date: 2026-11-09
- Work package: FB-73 (MDF-46). Migration `0015_credits.sql`. Crate `forgebase-credits`.
- Scope: docs/requirements/authz-entitlements-billing.md §E ("Credits and usage") and §F.3: prepaid
  balances, top-ups, grants, consumption from usage events, refunds, expiry, per-user and per-org
  ledgers, statistics; the balance never goes negative under concurrency.
- Builds on: ADR-0010 (usage events with `seq`, the billing seam, subjects), ADR-0008 §2 (subjects) and
  the policy pipeline, ADR-0007 (jobs, `ApiExtension`), ADR-0001 (one writer, error model).
- Leaves to later cards: selling credits (FB-70 calls `CreditSink`), the credits screens and payment
  statistics (FB-71), SDK helpers (FB-41).

## 1. Crate and changes elsewhere

`forgebase-credits` is its own crate on top of `forgebase-entitlements`: it owns its tables, a REST
surface and a job handler, and reads FB-24's usage events through `EntitlementService::events`.
Nothing below it depends on it.

| Crate | Change | Compatibility |
|---|---|---|
| `forgebase-migrations` | migration 15 `credits` (listed after 13; the migrator applies gaps) | append-only |
| `forgebase-server` | opens `CreditService` after the entitlement service, attaches the job queue, creates the schedule, registers the handler, mounts the extension; `RunningServer::credits()`; `ServerError::Credits` | additive |

No new third-party dependencies.

## 2. Model (migration 0015)

| Table | Holds | Mutable |
|---|---|---|
| `_forgebase_credit_accounts` | per subject: `balance` (`CHECK >= 0`) | balance only |
| `_forgebase_credit_ledger` | every top-up, grant, consumption, spend, refund, reversal and expiry; signed `amount` (sign checked per kind), `balance_after`, `seq` cursor, idempotency key unique per subject, the request fingerprint | **no** (triggers refuse UPDATE and DELETE) |
| `_forgebase_credit_lots` | what is left (`remaining`) of each top-up or grant, its `expires_at` | `remaining` only |
| `_forgebase_credit_allocations` | every signed change to a lot, by entry | **no** |
| `_forgebase_credit_rates` | price of a metered feature: `credits_per_unit` per `per_units` units, basis `units` or `overage` | yes (admin) |
| `_forgebase_credit_usage` | one row per FB-24 usage event charged: cost, charged, shortfall, entry | **no** |
| `_forgebase_credit_cursors` | the consumer's position in the usage feed (`seq`) | yes |

Subjects are those of ADR-0008 §2 (`org:<id>`, `user:<collection>/<id>`, `service:<id>`): a user's
personal credits and each organization's credits are separate accounts. Amounts are whole credits
(1 to 10¹² per entry); a product that sells fractions picks a smaller unit.

**Invariants**, true after every commit and checked by `CreditService::audit` (`GET /credits/audit`):

```
account.balance = SUM(ledger.amount) = SUM(lots.remaining) = balance_after of the newest entry ≥ 0
lot.remaining   = SUM(allocations of the lot)
entry.amount    = SUM(allocations of the entry)
```

The balance is derived (the ledger is the record) but stored, because the stored row is what the
concurrency check needs.

## 3. Writes

Each write is one transaction on the single writer, in this order:

1. **Idempotency.** Every write needs a key, unique per subject. A replay with the same request returns
   the first entry (`replayed: true`, HTTP 200 instead of 201); the same key with another request
   (amount, expiry, feature, target, external reference) is `409 idempotency_conflict`. Denials are not
   stored. Keys starting with `usage:` and `expiry:` are reserved for the service.
2. **Lazy expiry** of the subject's lapsed lots (an `expiry` entry per lot), so the balance equals what
   the open lots hold.
3. **The balance change.** A debit is `UPDATE accounts SET balance = balance - n WHERE subject = ? AND
   balance >= n`; no row changed means `402 insufficient_credits` (all or nothing). Because the check is
   in the `UPDATE`, a backend with concurrent writers cannot overdraw either; the `CHECK (balance >= 0)`
   is the last line.
4. **Lots.** Debits draw from the lots that lapse first (no expiry last), grants before top-ups, then
   oldest first, each draw again guarded (`WHERE remaining >= n`). Credits create a lot.
5. **The ledger row** with `balance_after`.

| Operation | Effect |
|---|---|
| top-up, grant | `+amount`, a new lot; grants (and top-ups) may lapse at `expires_at` |
| spend | `-amount` from the lots, all or nothing |
| consumption | `-min(cost, balance)` for a usage event (§4) |
| refund | `+amount` back to the lots the consumption or spend drew from (default: all that is left; capped at what the debit took less earlier refunds); units given back to a lot that lapsed meanwhile lapse again in the same transaction |
| reversal | takes back up to `amount` (default: all not yet reversed) of a top-up or grant **from what is left of its lot**; the rest is reported as `unrecovered` (already spent or lapsed), so a refunded payment never drives a balance negative |
| expiry | `-remaining` of a lapsed lot (lazily on the next write, and by the maintenance job) |

Reads report the spendable balance: the stored balance less lapsed lots not swept yet.

## 4. Credits paying for usage

Credits are separate from quotas (ADR-0010 §3): FB-24 still decides admission; credits price usage.
`credits.maintenance` (and `POST /credits/maintenance`) runs the consumer:

- read FB-24 usage events after the cursor (`seq` order, batches of 200, up to 50 batches a run);
- for each event of a priced feature (not gauges: levels are not consumption), units = all units or
  only `overage_units` (basis `overage`), cost = `ceil(units × credits_per_unit / per_units)` per event;
- skip events already in `_forgebase_credit_usage` (**idempotent by event id**), else debit
  `min(cost, balance)` as a `consumption` entry and record the charge with its `shortfall`;
- move the cursor to the batch's last `seq` in the same transaction.

The cursor can be moved (`PUT /credits/usage/cursor`, administrators): replaying the feed from 0 never
charges an event twice. Prices apply when an event is charged (within a minute), not retroactively.
The shortfall is what FB-70 may bill (`GET /credits/usage?shortfall=true`).

## 5. Expiry and jobs

`credits.maintenance` (schedule `credits-maintenance`, every minute at second 30) expires every lapsed
lot (batches of 500) and then charges new usage events. Expiry is also applied lazily per subject on
every write, so a lapsed credit is never spent even if the job is late.

## 6. Statistics

- **Balance history** (`GET /credits/stats/balance`): opening balance and, per hour, day or calendar
  month (UTC), credits added, removed and the closing balance (`balance_after` of the bucket's last
  entry; empty buckets carry the balance). At most 1000 buckets per query.
- **Consumption** (`GET /credits/stats/consumption`): per bucket and feature, credits debited
  (consumption and spend), refunded, net, units and entries; instance-wide for administrators.
- **Summary** (`GET /credits/stats/summary`): totals per kind, net change, balance now and usage
  shortfall over a window.

Default window: the last 30 days by day.

## 7. REST and authorization

Under `/api/v1/credits` (tag `credits`): `balance`, `lots`, `ledger[/{id}]`, `audit`, `top-ups`,
`grants`, `spend`, `refunds`, `reversals`, `rates[/{feature}]`, `usage[/cursor]`,
`stats/{balance,consumption,summary}`, `maintenance`.

| Operation | Who |
|---|---|
| balance, lots, ledger, entry, audit, usage charges, statistics | the subject itself (personal subjects), `billing:read` in the subject's organization or at realm level, or privileged callers |
| top-ups, grants, refunds of usage charges, reversals | `billing:write` **at realm level** (operators) or privileged callers (FB-70 through `CreditSink`, as the system principal). The org-level permission is not enough: an organization's owner or billing manager must not mint credits for it |
| spend, refunds of spends | `billing:write` or `usage:write` (trusted backends) in the subject's organization or at realm level; never implied for the subject itself |
| rates (write), cursor, maintenance, instance-wide statistics | platform administrators |
| rates (read) | any authenticated caller |

Another subject's entries are `404`. A debit the balance does not cover is `402 insufficient_credits`
with `balance` and `requested` in `details`.

## 8. FB-70 seam

`CreditSink` (implemented by `CreditService`, runs as the system principal):

- `credit_purchase(CreditInput)`: a top-up, idempotent by key (the payment or webhook event id),
  `external_ref` = order or invoice;
- `grant_credits(CreditInput)`: plan allowances and promotions, optionally lapsing (e.g. at the end of
  the billing cycle);
- `reverse_purchase(PurchaseReversal)`: finds the top-up by the purchase's key and reverses what is
  left of it (refund, chargeback); reports `unrecovered`.

## Consequences

- The ledger only grows; history is cheap to read by `seq` and by `(subject, at)`. Archiving old
  entries would need a checkpoint entry; not needed yet.
- Usage is charged after it happened (post-paid within a minute); a product that must refuse work
  without credits calls `spend` before the work (pre-paid) instead of pricing the feature. Admission by
  credits in the FB-21 pipeline (a step-6 hook) is a follow-up.
- Prices are per feature, not per plan; per-plan prices and multi-currency are FB-70's.
- Rates changed after usage happened apply to events not yet charged.
- Follow-ups: FB-70 wires `CreditSink` to purchases and bills `shortfall`; FB-71 screens; low-balance
  alerts through FB-40; a credit admission hook.
