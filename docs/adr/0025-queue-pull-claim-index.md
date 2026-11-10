# ADR-0025: Queue pull reads ready deliveries in index order

- Status: accepted
- Date: 2026-11-10
- Work package: FIX-9 (MDF-65)
- Scope: `crates/forgebase-queue` (claim query), migration `0025_queue_ready_order.sql`,
  `[realtime] max_connections_per_principal`.
- Builds on: ADR-0022 (queues and pub/sub).

## Decisions

1. **Pull cost no longer grows with the backlog.** The claim query filtered on
   `(subscription, state, available_at)` and ordered by `seq`, so SQLite read every ready row, joined each to its
   message and sorted them (`USE TEMP B-TREE FOR ORDER BY`) inside the write transaction. Migration 0025 adds
   `(subscription, state, seq)`; the query compares `+d.available_at` so the planner uses it and stops after
   `LIMIT` rows. Delivery guarantees (at-least-once, per-key ordering, ack deadlines) and every route and payload
   are unchanged: the same rows are chosen in the same order.
2. **Guard:** `pull_claim_query_does_not_sort_the_backlog` fails if the plan gains a temp B-tree or loses the
   index. The `queue_pull_ack_10` bench records the latency.
3. **Realtime per-principal cap is configuration.** `realtime.max_connections_per_principal`
   (`FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL`), default 32, range 1 to 1000000.
