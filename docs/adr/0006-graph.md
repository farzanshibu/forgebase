# ADR-0006: Graph model

- Status: accepted
- Date: 2026-11-08
- Work package: FB-51 (MDF-31)
- Scope: node and edge tables, traversal, shortest path, REST under `/api/v1/graph`.
- Builds on: ADR-0001, ADR-0003 (`AccessContext` / `Authorizer`).

## Decisions

1. **Storage**: two relational tables (migration 0006), `_forgebase_graph_nodes (tenant, id, label, properties)` and `_forgebase_graph_edges (tenant, id, type, source, target, directed, properties)`, `STRICT, WITHOUT ROWID`. Composite foreign keys `(tenant, source|target)` make an edge join nodes of its own tenant only. Indexes `(tenant, source, type, target)` and `(tenant, target, type, source)` serve both directions (a test checks the query plans). An undirected edge is stored once and walked both ways.
2. **Tenancy**: every statement filters on `AccessContext::tenant` (`''` when none). Another tenant's ids look like missing ones.
3. **Authorisation**: new `Action::GraphRead` / `Action::GraphWrite`, asked through `DataService::authorize` (now `pub`) before any data is touched. Graph data has no row-level policies: a `Decision::Filter` is treated as a denial. FB-21's policy engine plugs in behind the same trait.
4. **Traversal**: a recursive CTE over the edge indexes, depth 1 to 10, `UNION` (no repeated rows, so cycles end), a hard row cap, results capped at 1000. The recursive step uses `CROSS JOIN` to pin the walk queue as the outer loop; without it the planner scanned the edge table per queue row (37 s instead of 0.1 s on 10 000 edges).
5. **Shortest path**: breadth-first in Rust, one indexed query per level, bounded by `max_depth` (at most 10) and 100 000 visited nodes. Unweighted.
6. **All values are bound parameters**; SQL text comes only from fixed fragments. Ids, labels, edge types and filter keys are validated by character set.
7. **Crate layout**: `forgebase-graph` depends on data/db/core only (own extractor and error envelope), so `forgebase-api` can mount it (`nest_service`, inside the auth layer) and merge its OpenAPI fragment.

## Limits

Depth 10, results 1000, 20 edge types and 8 property conditions per query, properties 64 KiB, body 1 MiB. SQLite-specific SQL (recursive CTE, JSON1) stays in this crate; a PostgreSQL backend would port it.
