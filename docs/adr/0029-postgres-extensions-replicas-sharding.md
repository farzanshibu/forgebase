# ADR-0029: PostgreSQL extensions, read replicas and tenant sharding

- Status: proposed
- Date: 2026-11-11
- Work package: FB-158 (MDF-83, design); build packages FB-160 to FB-179 in
  [`docs/requirements/postgres-plan.md`](../requirements/postgres-plan.md#addendum-fb-158-extensions-replicas-and-sharding-fb-160-to-fb-179).
- Scope: which PostgreSQL extensions ForgeBase bundles and how each ForgeBase
  engine runs on them; read replicas (managed and external), read routing and
  failover; ForgeBase tenant sharding (control database plus shard servers);
  their configuration, admin API and dashboard screen; PostgreSQL as the
  default backend.
- Builds on: [ADR-0028](0028-bundled-postgres.md) (bundled engine, `Database`
  facade, single writer, backups, `db convert`) and
  [`rls-coverage.md`](../requirements/rls-coverage.md) (native RLS, roles).
  Engines that move: ADR-0005 (KV), ADR-0006 (graph), ADR-0007 and ADR-0023
  (jobs), ADR-0011 (realtime outbox), ADR-0012 (vector), ADR-0022 and
  ADR-0025 (queues).
- Supersedes: ADR-0028 decision 1 ("`sqlite` default") and ADR-0028 §1
  "Not shipped: `pg_cron`".

## Context

The human decided on 2026-11-11 (decisions 4–6 of the v0.3 cut in
`postgres-plan.md`):

4. PostgreSQL is the default backend for new projects. SQLite stays as an
   option: a config switch and the `-lite` download.
5. On PostgreSQL, features run on extensions: vector on pgvector, graph on
   Apache AGE (openCypher), queues on pgmq, scheduled jobs on pg_cron, and
   full-text and fuzzy search on built-in full-text search plus pg_trgm. All
   are bundled in the engine.
6. Read replicas and sharding ship in v0.3. ForgeBase manages read replicas;
   reads go to replicas and writes to the primary. Sharding is ForgeBase
   tenant sharding: each tenant or project lives on one of several PostgreSQL
   servers and ForgeBase routes to it. Citus is not used (AGPL-3.0).

What the code does today (read on 2026-11-11, `main` plus the uncommitted
FB-131 work in `worktrees/worker-fb131`):

- `Backend` has two data paths: `fetch_all` (read pool, writes fail with
  `ReadOnly`) and `begin` (the single writer) (`forgebase-db/src/backend.rs:242`).
  Nothing in the code knows about LSNs (log sequence numbers, PostgreSQL's
  position in its write-ahead log), replicas or more than one URL. FB-131's
  `PgDatabase` opens a writer pool of one connection and a read pool from one
  URL (`pg/mod.rs:125-182`).
- The server opens **one** `Database` and hands clones of it, or the one
  shared `DataService`, to every service (`forgebase-server/src/lib.rs:150`).
- Some reads on the read pool must see the latest commit: the catalog reload
  after DDL (`forgebase-data/src/collections.rs:209`) and the realtime
  dispatcher tailing the outbox (`forgebase-realtime/src/outbox.rs:117`).
  So "read pool = replica" is not safe; replica reads must be chosen per call
  site.
- Tenancy is realm → organization → project → environment, and the
  environment is the **tenant** (15-character id, `forgebase-schema/src/lib.rs:43`).
  The tenancy middleware sets `AccessContext.tenant` from
  `x-forgebase-tenant` after checking the policy snapshot
  (`forgebase-policy/src/http.rs:197`).
- Collection definitions are global (`_forgebase_collections` has no tenant
  column). A collection created with `tenant_scoped: true` gets a `_tenant`
  column; that flag cannot change later (`forgebase-data/src/collections.rs:234`).
  Relations are SQL foreign keys (`ddl.rs:89`) and stay inside one tenant
  (`records.rs:254`).
- System tables with a tenant column today: KV entries, graph nodes and edges,
  realtime outbox, vector collections. Jobs, queues and storage have none yet
  (gaps G3, G14 and G16 add them in FB-142, FB-140 and FB-141).
- Every background loop (job pool and schedules, queue maintenance and push,
  realtime dispatcher and pruner, KV flusher, vector warm-up and saver,
  functions after-hooks) works across all tenants of the one database.
- Realtime cross-instance fan-out exists as a trait but the server passes
  `None` (`forgebase-server/src/lib.rs:239`): one server process per database
  is the deployed model.
- Engines: vector keeps canonical float32 blobs in SQL and a derived USearch
  HNSW (hierarchical navigable small world) index in files; graph uses two
  tables and a recursive CTE (common table expression); queues fan out by
  cursor over a message log and keep per-subscription delivery rows; jobs fire
  schedules from a 250 ms Rust tick with idempotency keys; data-core search is
  FTS5 with every word quoted (implicit AND), no ranking, no fuzzy search.

## Decisions at a glance

| # | Decision |
|---|---|
| 1 | Each extension is an **accelerator** behind the existing service API: on PostgreSQL with the extension present, the engine runs on it; otherwise (SQLite, or an external server without it) the portable implementation runs. `[database.postgres.extensions]` sets `auto`/`on`/`off` per feature. |
| 2 | Pinned versions for PostgreSQL 18: **pgvector 0.8.7**, **Apache AGE 1.8.0 (PG18)**, **pgmq 1.13.1**, **pg_cron 1.6.8**, **pg_trgm** from contrib 18.6. All permissive licences; none GPL or AGPL. AGE has no Windows build: Windows engines ship without it. |
| 3 | **Vector**: canonical vectors in a pgvector column, one partial HNSW index per (collection, field). USearch and index files are not used on PostgreSQL. |
| 4 | **Graph**: ForgeBase's graph tables stay canonical on both backends; AGE holds a **derived, per-tenant graph** used for a new, read-only openCypher endpoint. It is rebuilt, never migrated or moved. |
| 5 | **Queues**: topics, cursors, filters, ordering, dedup, dead letters and replay stay in ForgeBase tables; each subscription's **delivery rows move into one pgmq queue**. Jobs do not use pgmq. |
| 6 | **Schedules**: pg_cron is the clock for every schedule whose cron expression it expresses identically; the rest keep the Rust tick. Missed firings still collapse into one. |
| 7 | **Search**: full-text on `tsvector` + GIN (ADR-0028 §7.4) with `plainto_tsquery`; pg_trgm adds a `trigram` index kind and an optional `fuzzy` list parameter (PostgreSQL only). |
| 8 | A **replica set** is one primary plus replicas. Replicas are **managed** (bundled engine, this host or another host through `forgebase node`) or **external** (a URL). |
| 9 | Replica reads are **opt-in per call site**; everything else stays on the primary. **Read-your-writes** by commit LSN tokens, waits up to `lsn_wait_ms`, then the primary. Lag limits take replicas out of rotation. |
| 10 | Failover in v0.3 is **manual promote** (fence, `pg_promote`, re-point, rejoin with `pg_rewind`). Automatic failover is v0.4. |
| 11 | Sharding: a **control database** (identity, policy, catalog, billing, topology) plus N **shards**; control is also shard 0. The **tenant (environment) is the placement unit**, projects kept together by default. |
| 12 | Every table has a **placement class** in one registry (control, tenant, per-set), checked by a test against the schema. Identity sequences on set *k* draw from the disjoint range [k·2⁴⁰, (k+1)·2⁴⁰), so rows keep their ids when they move and stay below 2⁵³ (safe in JavaScript). |
| 13 | **Cross-shard queries are refused**, except fixed admin aggregates (counts, stats) merged in Rust and realtime fan-in. Rules and filters may not follow a relation from a tenant-scoped into a global collection on a sharded cluster. |
| 14 | Collection DDL and policy regeneration run on control and every shard in **one two-phase commit** with a journal; index builds and system migrations roll out per node. |
| 15 | **Online tenant moves** use logical replication with row filters, a write freeze of at most 10 s, renumbering of order-defining tables, verification, then a placement flip. |
| 16 | Topology lives in control tables, managed through `/api/system/database/*`. Long operations are reserved **jobs** with progress. |
| 17 | The dashboard gets a **Database topology** screen in the **System studio** (Operate group). |
| 18 | `forgebase init` defaults to managed PostgreSQL in builds with the engine; existing SQLite projects never change on their own and move with `forgebase db convert`. |
| 19 | v0.3 supports **one ForgeBase server process per cluster**, as today. Several server processes need a routing fence and come with FB-157 (v0.4). |

## 1. Extensions

### 1.1 Rules shared by every extension

- **Accelerator, not a fork.** Every engine keeps its HTTP API, error codes,
  rules, tenancy and realtime behaviour. The extension changes how it stores
  or computes, never what callers see, except for additions that are marked
  PostgreSQL-only and answer `400 <feature>_unsupported` on SQLite.
- **Selection.** `[database.postgres.extensions]` has one key per feature
  (`vector`, `graph`, `queue`, `cron`, `trigram`) with `auto` (default: use
  it when the extension is installed or installable), `on` (refuse to start
  without it) or `off` (portable path). Managed mode always has them (Windows:
  not AGE). External mode checks `pg_available_extensions` at start; hosted
  providers often lack AGE and pgmq, and `auto` then falls back.
- **Who creates them.** Engine setup (FB-161) runs after migrations as
  `forgebase_super` (managed) or the configured URL's role (external):
  `CREATE EXTENSION IF NOT EXISTS … SCHEMA …`, then the PostgreSQL-only DDL
  each accelerator needs, versioned in `_forgebase_meta` as
  `ext.<feature>.schema = N`. Users cannot create, alter or drop these
  extensions (the SQL editor refuses it; FB-153's allow list covers the other
  contrib modules only).
- **Switching** an accelerator on or off on an existing database is an
  explicit conversion job (§1.4–§1.7 say what each one moves); it never
  happens because an extension appeared.
- **Visible**: `/api/system/info` gains `database.features.<feature> =
  {engine: "pgvector 0.8.7" | "portable", state}`; the Database Studio shows
  it (FB-155).
- **Tests**: the PostgreSQL CI job runs with accelerators on; one nightly job
  runs PostgreSQL with all of them `off`, so the portable path stays green
  on PostgreSQL too (FB-149).

### 1.2 Versions, licences and PostgreSQL 18 support

Checked 2026-11-11 against upstream releases, tags and licence files (Sources).

| Extension | Pinned | Licence | PostgreSQL 18 | Build | Runtime needs |
|---|---|---|---|---|---|
| pgvector | 0.8.7 (2026-10-01) | PostgreSQL License | since 0.8.1; CI tests 18 on Linux and macOS | PGXS `make OPTFLAGS=""` (no `-march=native`); Windows `nmake /F Makefile.win` | none; `CREATE EXTENSION vector` |
| Apache AGE | PG18/1.8.0 (vote passed 2026-08-05; ASF dist `age/PG18/1.8.0`) | Apache-2.0 (with PostgreSQL-licensed parts) | yes: 1.7.0 was the first PG18 port, 1.8.0 is current | PGXS; needs bison, flex, Perl at build time. **No Windows/MSVC build upstream** (issue #2502). musl not verified | `LOAD 'age'` per session or preloading; `ag_catalog` in `search_path` |
| pgmq | 1.13.1 (2026-10-20) | PostgreSQL License | since 1.8.0; CI 13–18 | pure PL/pgSQL since 1.3.2: copy the control and SQL files, no compiler, all five targets | none (no background worker) |
| pg_cron | 1.6.8 (2026-09-08) | PostgreSQL License (Citus Data; not AGPL) | since 1.6.6 | PGXS, links libpq; native Windows `Makefile.win` since 1.6.8 | `shared_preload_libraries`, one background worker, one database per cluster (`cron.database_name`) |
| pg_trgm | contrib of 18.6 | PostgreSQL License | yes (core contrib) | built with PostgreSQL | none; trusted extension |

Rejected: Citus (AGPL-3.0). Not needed: pg_partman (pgmq's partitioned queues
are not used).

**Apache AGE caveats** that shape §1.5:

- Row-level security (RLS) on label tables arrived in 1.7.0, but in 1.8.0
  variable-length and shortest-path traversals skip RLS and `SELECT` checks
  (fixed on `master` by apache/age#2561, not released for PG18), and
  `DETACH DELETE` under forced RLS can crash (#2475, `master` only).
  ForgeBase therefore does **not rely on AGE's RLS** in v0.3 (open question 1).
- Preloading `age` through `shared_preload_libraries` breaks `TRUNCATE`,
  `VACUUM FULL` and `CLUSTER` in databases without AGE (#2520; fix #2578 on
  `master` only). ForgeBase loads AGE per role instead:
  `ALTER ROLE forgebase_server SET session_preload_libraries =
  '$libdir/plugins/age'` (and the same for `forgebase_reader`), with
  `age.so` also installed under `lib/plugins/` so non-superusers may load it.
- 1.8.0 fixed variable-length queries on hot standbys (#2345); plain `MATCH`
  on a standby is not covered by upstream tests, so FB-163 adds that test
  before openCypher reads may go to replicas.

### 1.3 Engine build (FB-130 and FB-160)

Added to ADR-0028 §2.2:

- **`engine.lock`** pins each source tarball by SHA-256: pgvector tag
  tarball, pgmq release tarball, pg_cron tag tarball, AGE from the ASF dist
  (`apache-age-1.8.0-src.tar.gz`, its ASF SHA-512 checked once and the
  SHA-256 recorded).
- **pg_cron**: `make PG_CONFIG=<built pg_config> OPTFLAGS=""` on Linux and
  macOS, `nmake /F Makefile.win PGROOT=<install>` on Windows, linked against
  the engine's own libpq. Shipped: `lib/pg_cron.*`, `share/extension/pg_cron*`.
- **pgmq**: copy `pgmq.control` and `sql/pgmq--*.sql` into `share/extension/`
  on all targets. No compiled code.
- **pg_trgm**: already in the contrib build; add it to the shipped list.
- **AGE** (FB-160, separate because it is the risky one): PGXS build on
  Linux (Alpine) and macOS with bison and flex from the build image; install
  `age.so` into `lib/` and `lib/plugins/`. Windows archives omit it and say
  so in the archive manifest (`features.graph_cypher = false`).
- **Smoke test** per target, added to ADR-0028 §2.2: `CREATE EXTENSION` for
  vector, pg_trgm, pgmq, pg_cron (and age where shipped); one HNSW query;
  `pgmq.send`/`read`; a pg_cron job that fires within 70 s; one Cypher
  `MATCH`; `pg_basebackup` with a standby started from it.
- **Licences**: `share/licenses/{pgvector,age,pgmq,pg_cron}.txt` and AGE's
  `NOTICE`; the CI licence check of ADR-0028 §2.4 covers them.
- **Size**: AGE adds about 3–4 MB unpacked, pg_cron under 0.5 MB, pgmq under
  0.3 MB (estimates; FB-130/FB-160 measure).
- **Engine settings** written by FB-132 (`forgebase-postgres.conf`, deny-listed
  for users): `shared_preload_libraries = 'pg_stat_statements,pg_cron'`,
  `cron.database_name = 'forgebase'`, `cron.use_background_workers = on`
  (no libpq loopback login needed), `cron.log_run = off` (ForgeBase keeps its
  own attempt history), `max_worker_processes = 16`, **`wal_level = logical`**
  (tenant moves, §3.8; changing it later needs a restart, so it is the default
  from the start), `max_wal_senders = 16`, `max_replication_slots = 16`,
  `max_prepared_transactions = 16` (§3.7), `hot_standby_feedback = on` on
  replicas.

### 1.4 Vector on pgvector

- **Storage.** `_forgebase_vector_values` gets a column `vec vector` (no fixed
  dimension) added by engine setup; the portable `vector bytea` column stays
  `NULL` on accelerated databases. pgvector stores `float4`, so the stored
  vector is the returned vector, as ADR-0012 §1 requires.
- **Index.** One partial expression index per (collection, field), matching
  ADR-0012's one-index-per-field model:
  `CREATE INDEX CONCURRENTLY fbv_<collection id>_<field hash> ON
  _forgebase_vector_values USING hnsw ((vec::vector(<dim>)) <ops>) WITH (m =
  <connectivity>, ef_construction = <expansion_add>) WHERE collection_id =
  <id> AND field = '<field>'`; queries repeat the cast and the predicate so
  the planner uses it. Metric `cosine`/`l2`/`dot` → `vector_cosine_ops` /
  `vector_l2_ops` / `vector_ip_ops`. Element type `f16` → `halfvec(<dim>)`
  ops; `i8` (no pgvector type) → `halfvec`, documented. pgvector indexes
  `vector` up to 2 000 dimensions and `halfvec` up to 4 000: wider `f32`
  fields are indexed as `halfvec`, fields over 4 000 have exact search only.
- **Distances** keep ADR-0012's definitions: cosine `<=>` (= 1 − cos), L2
  `<->`, dot `1 + (a <#> b)` (pgvector returns the negative inner product).
- **Strategies** keep their names and rules (ADR-0012 §4): `exact` orders by
  the distance on the uncast column (no index); `ann` uses the index with
  `SET LOCAL hnsw.ef_search = <expansion_search or params>`; `filtered_ann`
  and `post_filter` use pgvector 0.8 iterative scans (`SET LOCAL
  hnsw.iterative_scan = relaxed_order`, results re-sorted) with the compiled
  filter in the same `WHERE`; the over-fetch loop of ADR-0012 stays as the
  fallback when fewer than `k` rows return.
- **Rebuild** = `REINDEX INDEX CONCURRENTLY` on a maintenance connection (DDL
  only, not the writer), progress from `pg_stat_progress_create_index` into
  the existing stats (`done`/`total`); `recall` compares the two orders in
  SQL. No index files, no save loop, no warm-up, no in-memory index on
  PostgreSQL.
- **Hybrid search**: the keyword side uses the `tsvector` column FB-147 adds
  to items (`plainto_tsquery`, `ts_rank`); reciprocal rank fusion unchanged.
- **Tenancy and rules** unchanged: collection ids resolve from (tenant, name);
  FB-139's collection rules compile into the same `WHERE`.
- **Replicas**: HNSW indexes are WAL-logged, so search, hybrid, get and list
  may run on replicas (§2.4).
- **Conversion** (`vector.convert`, job): bytea → `vec` in batches, then
  build indexes; back to portable: `vec` → bytea, then USearch rebuilds.
- **SQLite and portable PostgreSQL**: unchanged (USearch, ADR-0012).

### 1.5 Graph on Apache AGE (openCypher)

Why ForgeBase's tables stay canonical: AGE identifies vertices by `graphid`
per label table, so ForgeBase's string ids unique across labels, undirected
edges, the tenant column and the composite foreign keys (ADR-0006 §1) would
each need side tables; AGE's RLS gaps (§1.2) and its missing Windows build
would make the REST graph API behave differently per platform; and AGE label
tables use sequences, which PostgreSQL 18 logical replication does not copy
(§3.8). A derived graph has none of these problems: it is rebuilt from the
canonical tables, as ADR-0001 treats every derived index.

- **REST API** (`/api/v1/graph`, 8 routes) keeps reading and writing
  `_forgebase_graph_nodes/edges` (FB-147's PostgreSQL port), byte-identical
  on both backends.
- **Derived graph per tenant**: AGE graph `fbg_<first 16 hex of
  sha256(tenant)>` (`fbg_shared` for the `''` tenant). Vertex label =
  ForgeBase label, edge label = edge type; names AGE cannot represent (longer
  than 63 bytes, or not a valid label) go to the catch-all labels `_fbnode` /
  `_fbedge` with `__label` / `__type` properties. Properties are the ForgeBase
  JSON plus `__id`; undirected edges carry `__undirected: true`. A map table
  `_forgebase_graph_age_ids (tenant, kind, id, age_id)` links the two.
- **Materialised on first use**: the first openCypher request for a tenant
  starts the job `graph.materialise` (progress in stats) and answers `409
  graph_materialising` with the job id; after it finishes, the graph service
  writes the AGE rows in the **same write transaction** as every canonical
  change (direct `INSERT/UPDATE/DELETE` on the label tables, never Cypher
  writes). Tenants that never use openCypher cost nothing. `POST
  /graph/cypher/drop` frees a tenant's derived graph.
- **Endpoint** `POST /api/v1/graph/cypher {query, params, limit ≤ 1000,
  timeout_ms ≤ 30 000}` → `{columns, rows, truncated}` (PostgreSQL with AGE
  only; otherwise `400 cypher_unsupported`):
  - read-only: runs in a `READ ONLY` transaction, so any write clause fails
    in the engine;
  - permission `engine:graph_cypher` (new, grantable, not implied by
    `engine:graph_read`);
  - a small tokenizer (defence in depth, not the boundary) refuses write
    clauses, `CALL`, schema-qualified function calls and the transaction's
    random dollar-quote tag, and counts the final `RETURN` items for AGE's
    required `AS (c1 agtype, …)` list;
  - executes `SELECT * FROM ag_catalog.cypher('<graph of ctx.tenant>',
    $fb<random>$ … $fb<random>$, $1) AS (…) LIMIT n` with `SET LOCAL
    search_path = ag_catalog, pg_catalog`, `statement_timeout`, and, for
    non-privileged callers, `SET LOCAL ROLE forgebase_rls` with claims;
  - **tenant isolation is structural**: the graph name comes from
    `ctx.tenant`, never from input, and each graph holds one tenant's data;
  - callers to whom any graph label rule (G10, FB-139) applies get `403
    cypher_rules_unsupported` in v0.3, because AGE 1.8.0 cannot enforce row
    rules inside traversals (open question 1).
- **Replicas**: Cypher reads go to replicas once FB-163's standby test
  passes; until then, the primary.
- **Moves, backups, convert**: the derived graph is not moved (§3.8); it is
  marked stale and rematerialised on the target on next use. Physical backups
  include it; `db convert --to sqlite` drops it (nothing is lost).
- **Windows and external servers without AGE**: REST graph works; openCypher
  answers `400 cypher_unsupported`.

### 1.6 Queues on pgmq

pgmq is a visibility-timeout queue (send, read with `FOR UPDATE SKIP LOCKED`,
set visibility, delete, archive). ForgeBase queues are publish/subscribe with
fan-out by cursor, filters, ordering keys, dedup, dead letters, replay,
retention and push (ADR-0022). pgmq maps onto exactly one part of that: a
subscription's **owed deliveries**.

- **Unchanged ForgeBase tables** on both backends: topics, messages (the
  log, `seq` order), subscriptions with `cursor_seq`, dedup, dead letters,
  snapshots, rates, state.
- **On PostgreSQL with pgmq**: each subscription has a pgmq queue
  `fbs_<subscription id>` (created and dropped with the subscription;
  `pgmq.create`/`pgmq.drop_queue`). Materialisation sends delivery entries
  with `pgmq.send_batch` instead of inserting into
  `_forgebase_queue_deliveries`. Message body `{seq, key, lease, history}`;
  pgmq's `read_ct` is the attempt count, `vt` is `available_at` or
  `lease_until`.
- **Claim**: ForgeBase's own statement over the pgmq queue table, which is
  pgmq's `read` plus two things pgmq does not do (lease tokens and
  ordering keys): `UPDATE pgmq.q_fbs_<id> SET vt = now() + deadline, read_ct
  = read_ct + 1, message = jsonb_set(message, '{lease}', $lease) WHERE msg_id
  IN (SELECT msg_id … WHERE vt <= now() [AND no earlier msg_id with the same
  key] ORDER BY msg_id LIMIT $n FOR UPDATE SKIP LOCKED) RETURNING …`.
  Ack = `DELETE … WHERE msg_id = $1 AND message->>'lease' = $2` (a late ack
  stays valid until the entry is leased again, as ADR-0022 §4 says); nack and
  modify-deadline update `vt` and history. Dead lettering deletes the entry
  and writes ForgeBase's dead-letter record as today.
- **Table layout check**: FB-164 pins the pgmq queue-table columns it relies
  on (`msg_id`, `read_ct`, `enqueued_at`, `vt`, `message`) and checks them at
  start; a mismatch falls back to the portable path with a warning.
- **Wake-ups**: in-process `watch` as today; `pgmq.enable_notify_insert` is
  not used in v0.3 (one server process).
- **Jobs stay on ForgeBase SQL**: priorities, named-queue budgets,
  workflows, leases with fencing tokens and checkpoints have no pgmq
  equivalent.
- **Limits**: two tables per subscription (`q_` and pgmq's unused `a_`);
  `[queue] max_subscriptions` (default 10 000) caps the table count.
- **Replicas**: pgmq writes, so claims stay on the primary; metrics and the
  message browser may read replicas.
- **Conversion** (`queue.convert`): moves owed deliveries in either direction
  (a backlog, not history).

### 1.7 Schedules on pg_cron

- **Mapping**: a schedule whose expression pg_cron reads identically becomes
  `cron.schedule('fbsched_<id>', '<expr>', $$SELECT
  forgebase.fire_schedule(<id>)$$)`; 6-field expressions of the form `*/N * *
  * * *` map to pg_cron's `'N seconds'` with due times aligned to the N-second
  grid. FB-165 decides "identically" by a differential test (the next 1 000
  due times from ForgeBase's parser and from pg_cron's semantics, including
  day-of-month/day-of-week logic, names and `7` = Sunday); everything else
  stays on the Rust tick, marked `fired_by = 'forgebase'`.
- **`forgebase.fire_schedule(id)`** (PL/pgSQL, migration pair from FB-165):
  locks the schedule row, computes the due time, inserts the job with
  idempotency key `schedule:<id>:<due ms>` (`ON CONFLICT DO NOTHING`), and
  records `last_run_at`/`last_job_id`, exactly what `fire_tx` does today.
  The Rust scheduler never fires a pg_cron-owned schedule, so the two never
  race. `next_run_at` of pg_cron schedules is computed when read.
- **Missed firings collapse into one** (ADR-0007 §6): pg_cron does not catch
  up, so at engine start the Rust scheduler fires each pg_cron-owned schedule
  once if a due time passed since `last_run_at`.
- **Wake-up**: the fire function runs `NOTIFY forgebase_jobs`; the server
  listens on a dedicated connection (sqlx `PgListener`) and wakes the pool,
  instead of waiting for the next 250 ms poll.
- **Second writer**: pg_cron's background worker writes only schedule and
  job rows through this function; FB-146's "no second writer" test allows
  exactly that.
- **pg_cron is not a user feature in v0.3**: the SQL editor refuses `cron.*`,
  because pg_cron jobs would run SQL without ForgeBase's rules.
- **Replicas and shards**: pg_cron runs only on a primary (it idles in hot
  standby and starts on promotion), once per server, in the `forgebase`
  database. Each shard's pg_cron fires that shard's schedules.

### 1.8 Full-text and fuzzy search

- **Full text** as ADR-0028 §7.4 (generated `tsvector`, GIN), with one
  correction: ForgeBase quotes every word and ANDs them, so PostgreSQL uses
  `plainto_tsquery('simple', unaccent($1))`, not `websearch_to_tsquery`
  (which would interpret operators in user input). No ranking is added to
  record lists (there is none today).
- **pg_trgm**:
  - new index kind `trigram` (`CREATE INDEX … USING gin (<col>
    gin_trgm_ops)`), which makes the `~` (contains) filter, compiled to
    `ILIKE '%…%'` on PostgreSQL, use an index;
  - optional list parameter `fuzzy=<text>` with `fuzzy_fields` (each must
    have a trigram index): `WHERE <col> % $1 ORDER BY similarity(<col>, $1)
    DESC, id`, threshold `pg_trgm.similarity_threshold` (default 0.3, per
    request `fuzzy_threshold`);
  - SQLite answers `400 index_kind_unsupported` / `400 fuzzy_unsupported`,
    and `~` keeps working there, unindexed.
- **Vector hybrid** keyword ranking uses `ts_rank` (§1.4).

### 1.9 What does not move

- **KV** stays memory-authoritative with write-behind rows (ADR-0005); no
  extension. Its rows follow the tenant's shard (§3.5).
- **Realtime outbox** stays a table written in the data change's
  transaction; commit order equals sequence order because each database
  keeps one writer (ADR-0028 decision 6).
- **Jobs claims** (§1.6).

### 1.10 Changes by crate (extensions)

| Crate | Change |
|---|---|
| `forgebase-db` | `pg/extensions.rs`: detection, modes, engine setup, versioned extension DDL, `info()` per feature (FB-161) |
| `forgebase-pgengine` | settings of §1.3; per-role `session_preload_libraries` for AGE (FB-132 scope, FB-161) |
| `forgebase-vector` | `pg/` store: pgvector column, partial HNSW indexes, iterative scans, concurrent rebuilds; `AnnEngine` not used on PostgreSQL (FB-162) |
| `forgebase-graph` | `age/`: derived graph, map table, same-transaction sync, materialise job, `/graph/cypher`, tokenizer (FB-163) |
| `forgebase-queue` | `pgmq/`: delivery store over pgmq queues, claim/ack/nack, conversion (FB-164) |
| `forgebase-jobs` | `pgcron.rs`: expression mapping, sync of `cron.job`, catch-up, `LISTEN` wake (FB-165); migration pair with `forgebase.fire_schedule` |
| `forgebase-data`, `forgebase-query` | `trigram` index kind, `fuzzy` parameter, `plainto_tsquery` (FB-166) |
| `forgebase-api` | `/api/system/info` features; OpenAPI for the new parameters and the Cypher route |
| `forgebase-kv`, `forgebase-realtime` | none |

## 2. Read replicas

### 2.1 Model

- A **replica set** is one primary and zero or more replicas. Without
  sharding there is one set, `control`. With sharding every shard is a set
  too (§3.11).
- A **node** is one PostgreSQL server:
  - `local`: a managed engine on this host, a child process of the server
    (ADR-0028 §4);
  - `remote`: a managed engine on another host, run by `forgebase node` (§2.3);
  - `external`: a URL the operator runs (hosted read replicas, own servers).
- The default deployment is one set with one `local` primary: nothing below
  changes behaviour until a replica is added.

### 2.2 Managed replicas on this host

- Layout: `<data_dir>/postgres/nodes/<node>/data` and its own socket
  directory; the same extracted engine.
- Provisioning (job `database.node.add`): create the physical slot
  `fbr_<node>` on the primary; `pg_basebackup -D … -X stream -R -S fbr_<node>
  --checkpoint=fast` as `forgebase_replicator` (new role: `LOGIN
  REPLICATION`, password in `roles.secret`) over the primary's socket;
  ForgeBase writes the replica's `forgebase-postgres.conf` (`hot_standby =
  on`, `hot_standby_feedback = on`, `max_standby_streaming_delay = 30s`,
  `primary_slot_name`); start; wait for `streaming` in
  `pg_stat_replication`.
- WAL kept for slots is bounded: ForgeBase sets `max_slot_wal_keep_size` to
  8 GB (PostgreSQL's default is unlimited) and PostgreSQL 18's
  `idle_replication_slot_timeout` to 1 day (PostgreSQL's default is off). A
  replica that falls further behind is invalidated and shown
  `needs_rebuild`, instead of filling the primary's disk.
- Supervision, restart and logs as the primary (ADR-0028 §4.3); logs tagged
  with the node name.
- Honest limit, shown in the dashboard: a replica on the same host adds read
  capacity and a fast promote target, not protection against losing the host.

### 2.3 Managed replicas on other hosts (`forgebase node`)

- On the other host: `forgebase node join <server url> --token <join token>
  --data-dir DIR` with the same ForgeBase binary (the engine is embedded). It
  runs engine-only: no HTTP API, no services.
- `POST /api/system/database/join-tokens {set, role: "replica"}` returns a
  one-time token (15 minutes, sealed in control, audit-logged) and the exact
  command. The agent calls the server over HTTPS with it and receives its
  node id, the primary's replication address and the replicator credentials.
  The engine version (major, minor, build) must match the primary's exactly,
  or the join is refused.
- The primary must listen on TCP: `[database.postgres] listen` is set and
  ForgeBase generates `pg_hba.conf` entries `hostssl replication
  forgebase_replicator <agent address>/32 scram-sha-256` and `hostssl
  forgebase forgebase_reader <agent address>/32 scram-sha-256` (FB-153's
  generator and certificate). The replica listens on TCP with TLS for the
  server's read pool.
- The agent keeps an outbound long-poll to the server for commands
  (`rebuild`, `stop`, `promote`, `rejoin`) and sends a heartbeat every 5 s
  (state, replay LSN, disk free, engine version). Outbound only: no inbound
  port on the agent host beyond PostgreSQL's.

### 2.4 External replicas

- Declared in the file (`[[database.nodes]]`, URL from an environment
  variable) or through the API (URL sealed with the auth keyring, like billing
  provider secrets).
- Checked before use: `pg_is_in_recovery()` is true, `system_identifier`
  (`pg_control_system()`) equals the primary's, version ≥ 16, and the role
  can read the ForgeBase schemas. A mismatch is `400 node_mismatch`.
- ForgeBase routes reads to them and watches lag; it does not promote,
  rebuild or back them up.

### 2.5 Which reads may go to a replica

Replica reads are opt-in. `Backend` gains a default method
`fetch_routed(stmt, &ReadIntent)` that calls `fetch_all` (primary) unless the
backend is a routed set; only the call sites below pass an intent.

| May read a replica | Always the primary |
|---|---|
| record list, view, search, aggregate, expand, versions | anything inside a write transaction |
| vector get, list, search, hybrid, stats | catalog reload after DDL; policy snapshot load |
| graph get, list, neighbors, traverse, shortest path, stats; Cypher (§1.5) | auth: sessions, tokens, API keys, MFA, lockouts, revocation |
| storage object and bucket metadata reads (not signed-URL issue) | billing, credits, entitlements (balances and checks) |
| queue message browser and metrics; job list, get, events, attempts, stats | queue and job claims, acks, heartbeats; realtime outbox tail and replay |
| dashboard statistics; SQL editor reads when "Read from replica" is ticked | migrations, backups, `doctor` integrity checks |

### 2.6 Read-your-writes

- **Commit LSN.** When a set has replicas, the backend reads
  `pg_current_wal_insert_lsn()` on the writer connection right after each
  `COMMIT` (one extra round trip; skipped without replicas). That LSN is at
  or after the commit record, so a replica that has replayed it shows the
  write.
- **Tokens.** Responses to writes carry `X-ForgeBase-Commit:
  <set>/<lsn>`. The JavaScript and Rust SDKs keep the highest token per set
  and send `X-ForgeBase-Read-After: <token>` on later reads. Without an SDK
  (the dashboard, curl), the server also keeps the last commit LSN per
  (principal, set) for 10 s (LRU, 10 000 entries). The effective minimum is
  the higher of the two.
- **Realtime.** Each dispatched page of events carries the primary's
  `pg_current_wal_lsn()` sampled when the page was read; SDKs feed it into
  the same token, so "event received, then read the record" sees the record.
- **Routing.** A replica is eligible when it is healthy, within the lag
  limits and its replay LSN ≥ the minimum. If the best replica is behind, the
  router polls it (`SELECT pg_last_wal_replay_lsn() >= $1`, every 5 ms) for up
  to `lsn_wait_ms` (default 50 ms), then reads the primary. PostgreSQL 18 has
  no server-side wait (`pg_wal_replay_wait` was reverted before 18); with an
  engine ≥ 19, ForgeBase uses `WAIT FOR LSN … WITH (TIMEOUT …)` instead.
- `X-ForgeBase-Consistency: strong` (SDK option `consistency: "strong"`)
  sends a request's reads to the primary.

### 2.7 Lag limits, health and fallback

- A monitor per set samples every `health_interval_ms` (500): on the primary,
  `pg_stat_replication` (`replay_lsn`, `replay_lag`) for streaming replicas;
  for external replicas, `pg_last_wal_replay_lsn()` on the replica against the
  primary's `pg_current_wal_lsn()`.
- A replica above `max_lag_ms` (2 000) or `max_lag_bytes` (64 MiB) leaves
  rotation (`lagging`) and returns after three samples under half the limit.
- States: `provisioning`, `catching_up`, `streaming`, `lagging`,
  `unreachable`, `needs_rebuild`, `promoting`, `retired`.
- Choice among eligible replicas: weighted, fewest in-flight reads.
- Fallback: a replica read that fails with `Unavailable`, or is cancelled by a
  recovery conflict (SQLSTATE `40001` on a standby), is retried once on the
  primary. With every replica out, all reads use the primary; nothing fails
  because a replica is gone.

### 2.8 Failover: manual promote in v0.3

**Decision: manual.** Reasons:

- ForgeBase is one server process and the only writer (decision 19). When the
  primary's host fails, a `local` setup loses the API server with it;
  promoting a database alone does not restore service.
- Safe automatic failover needs fencing and a quorum (a third voter) so that
  two primaries never accept writes; without them a network split loses data.
  That is Patroni-and-etcd territory and belongs with multi-process servers
  (v0.4).
- A manual promote is one confirmed action that takes seconds, which suits the
  deployments v0.3 targets.

**Promote** (job `database.promote`, typed confirmation of the node name):

1. Pre-check the target; show its replay LSN against the last known primary
   LSN as the data-loss estimate.
2. Fence the old primary: close the writer pool; stop it (`local`: child
   stop; `remote`: agent command). If it is unreachable, the operator must
   tick "the old primary is down" (`accept_data_loss: true`). External
   primaries cannot be fenced: promote is refused for external sets.
3. If the old primary stopped cleanly, wait until the target has replayed its
   final LSN (no data loss).
4. `SELECT pg_promote(true, 60)` on the target.
5. Re-point the writer and read pools; swap roles in the control tables;
   bump the topology epoch; rewrite `topology.json` (§4.2).
6. Other replicas: new slots on the new primary, `primary_conninfo` changed
   and reloaded (no restart needed); they follow the new timeline.
7. The old primary becomes `needs_rebuild`; "Rejoin" runs `pg_rewind`
   (possible because `initdb` uses `--data-checksums`) and starts it as a
   replica, falling back to a fresh clone.

### 2.9 Backups with replicas

- `[backup] source = "primary" | "replica"` (default `primary`, as today);
  `replica` runs `pg_basebackup` against a healthy replica of the set, which
  PostgreSQL supports, so backups do not load the primary. The manifest
  records the node, timeline and end LSN.
- A restore rewrites the primary's history, so every replica of that set
  becomes `needs_rebuild` and is not routed to until rebuilt: managed ones
  automatically, external ones when the operator confirms the rebuild.
- Point-in-time recovery stays FB-156 (v0.4); its WAL archive comes from the
  primary.

## 3. Tenant sharding

### 3.1 Model

- The **control set** holds the control database: identity and sessions,
  administrators, policy and the tenancy tree (realms, orgs, projects,
  environments), collection catalog and rules, functions, billing, credits,
  entitlements, customize, feature configuration and the topology tables.
- **Shard sets** 1…N hold tenant data. The control set is also **shard 0**: a
  single-server deployment is "control hosts every tenant", and adding shards
  moves tenants out. Control can stop hosting tenants (`hosts_tenants =
  false`) once drained; it cannot be removed.
- **Placement unit: the tenant (environment)**. New environments of a project
  go to the shard of the project's existing environments (project affinity,
  default on), so "a project lives on one shard" holds unless an operator
  moves one environment.
- Every node has the **full system schema** (the same migrations). The
  placement class of each table (§3.2) decides which node's copy holds rows.

### 3.2 Placement classes

One registry, `forgebase_db::placement::TABLES`, lists every system table
with its class, its tenant key and its move strategy. A test fails if a table
in the PostgreSQL schema is missing from it, the same way the schema-parity
test keeps the two schemas honest.

| Class | Rows live on | Examples |
|---|---|---|
| `control` | control only | admins, auth keys, sessions, API keys, realms, orgs, projects, environments, roles, collection catalog, policies, functions, billing, credits, entitlements, customize, topology |
| `tenant` (column) | the tenant's shard | collection tables with `tenant_scoped: true` (`_tenant`), KV entries, graph nodes and edges, vector collections (`tenant`), jobs, queue topics and subscriptions, storage buckets and objects once G3/G14/G16 add `tenant` |
| `tenant` (parent) | the shard of the parent row's tenant | vector items and values (via `collection_id`), job events and attempts (via `job_id`), queue messages, deliveries, dedup, snapshots (via topic or subscription) |
| `per_set` | each set keeps its own (its replicas copy it) | realtime outbox, queue relay and function after-hook cursors, `_forgebase_migrations`, node-local meta |
| `control_replicated` | control, read-only copies on every shard | record ACL (`_forgebase_resource_permissions`), direct-login claims (`_forgebase_db_logins`); used by native RLS functions on shards |

- Rows whose tenant is `NULL` or `''` (instance-wide topics, jobs and buckets
  without a tenant, collections without `tenant_scoped`) are `control`.
- `control_replicated` tables reach shards by logical replication
  (publication `fb_control`, one subscription per shard). Lag is sub-second;
  the API path does not read them on shards (it uses the policy snapshot),
  only native policies for direct logins and `enforce` mode do.

### 3.3 Identities

- Identity sequences on set *k* are created with `START k·2⁴⁰ + 1 MINVALUE
  k·2⁴⁰ + 1 MAXVALUE (k+1)·2⁴⁰ − 1` (control is k = 0, so every existing id,
  including those from `db convert`, is valid). Up to 8 191 sets fit below
  2⁵³, so ids and realtime cursors stay exact in JavaScript.
- A row keeps its id when its tenant moves; ids never collide, because each
  set only allocates in its own range.
- Order-defining tables (where clients see sequence order: queue message
  `seq`, job events, job attempts) are **renumbered** on the target in
  source order during a move (§3.8), with references remapped.
- A realtime cursor shows the set it came from (`cursor >> 40`). After a move
  an old cursor is from another set, so the server answers `reset` with
  reason `tenant_moved` instead of a silent gap.

### 3.4 Placement

- `_forgebase_tenant_placements (tenant, set_id, epoch, state, moved_at)`;
  `state` is `active`, `moving`, `frozen` or `cleanup`.
- New tenant: project affinity, otherwise the active shard with the lowest
  load score (estimated bytes ÷ weight; a stats job refreshes per-tenant row
  counts and sizes hourly). `POST environments` accepts an optional `shard`
  for administrators.
- The server caches the map in memory and reloads it on every placement
  change (same process; decision 19).

### 3.5 Routing

`forgebase-db` gains `DbRouter`: `control()`, `for_tenant(&TenantRef) ->
SetHandle` and `sets()`. `SetHandle` implements `Backend` (writer on the set's
primary, routed reads per §2). Without sharding every call returns the single
set, so SQLite and single-server PostgreSQL behave exactly as today.

| Path | Routed by |
|---|---|
| HTTP requests | `ctx.tenant` after the tenancy middleware (`forgebase-policy/src/http.rs:197`); no tenant → control |
| Data core | `DataService` resolves per call: tenant-scoped collection → tenant's set; other collections → control |
| Edge functions | host calls go through `DataService` with the function's context; its tenant comes from the caller (FB-93) or the job (FB-142) |
| Jobs | the job's `tenant`; the pool runs one claim loop per set; HTTP workers without a tenant claim round-robin across sets; `GET /jobs/{id}` tries the caller's set, then all sets by primary key |
| Schedules | stored with their tenant; fired on that set (pg_cron per primary, or the Rust tick per set) |
| Queues | topic's tenant; instance-wide topics on control. A record-source topic on control relays from every set's outbox, with a per-set cursor stored on control in the same transaction as the inserts (dedup id `outbox:<set>:<seq>`) |
| Realtime | one dispatcher and pruner per set; the bus merges them. Replay works for subscriptions that map to one set (tenant-bound, or global collections); a privileged subscription spanning sets gets live events without replay (`cursor_unsupported_sharded`) |
| KV | memory as today; the flusher groups dirty keys by set; start-up loads every set |
| Vector, graph, storage | the tenant's set |
| Background loops | one instance per set; loops skip tenants that are `moving` in or out of their set (an exclusion list in the claim and maintenance SQL) |
| Control-only services | auth, policy, billing, credits, entitlements, customize, functions registry: `control()` |

### 3.6 Cross-shard queries

- A tenant-bound caller only ever touches one set, so it never needs one.
- A privileged caller listing a tenant-scoped collection **without** a tenant
  on a sharded cluster gets `400 tenant_required_sharded` (today such a call
  sees every tenant's rows; on one set it still does).
- Allowed fan-outs, merged in Rust: dashboard statistics and counts, health,
  `GET /jobs/{id}` lookup, privileged realtime fan-in.
- The SQL editor runs on one node, chosen in its node selector.
- **Relations across placements**: tenant-scoped collections may still
  reference global collections (for example `posts.author → users`). On a
  sharded cluster such a relation has no SQL foreign key; the data core checks
  existence (it already does, `records.rs:254`), `expand` fetches targets
  from control in a second query, and `ON DELETE` actions from control rows
  run as a fan-out job. Rules and filters that **follow** such a relation
  (`author.team = …`) are refused when saved and when queried
  (`relation_crosses_shards`). Adding the first extra shard runs this check on
  every rule and lists offenders first (open question 2).

### 3.7 Schema changes and migrations across shards

- **Collection DDL** (create, update, delete collection; rule changes that
  regenerate native policies) runs on control (catalog rows) and on every
  shard (DDL and policies) as one two-phase commit: each node `PREPARE
  TRANSACTION 'fb_<uuid>_<set>'`; when all are prepared, the coordinator
  commits a journal row in control (`_forgebase_xact_journal`: gid, nodes,
  state), then `COMMIT PREPARED` everywhere. If anything fails before the
  journal row, all prepared parts roll back. At start and every minute,
  recovery resolves in-doubt transactions (`pg_prepared_xacts`) from the
  journal. Managed nodes set `max_prepared_transactions = 16`; external nodes
  must have it above 0, or sharding is refused (`400 sharding_unsupported`).
- **Index builds** (`CREATE INDEX CONCURRENTLY` cannot run in a
  transaction) roll out per shard: the catalog row records state per set
  (`building` → `ready`); the index manager shows it.
- **System migrations**: control first, then every shard (at most 4 at a
  time), each with its own `_forgebase_migrations` and pre-migration snapshot.
  A set below the binary's head is `migrating`; its tenants get `503
  shard_migrating` until it is done. The paired-file format is unchanged.
- A new shard replays the catalog (every collection's DDL and policies) and
  sets its sequence ranges before it becomes `active`.

### 3.8 Moving a tenant online

Job `database.tenant.move` (`from`, `to`, tenant), states `preparing →
copying → catching_up → switching → cleanup → done` or `aborted`.

1. **Prepare.** Target at schema head. Tenant `state = moving`; schema
   operations for that tenant (new vector collections, graph
   materialisation) answer `409 tenant_moving`. On the source, tables whose
   replica identity does not cover the tenant key get `REPLICA IDENTITY FULL`
   for the move (PostgreSQL requires row-filter columns to be in the replica
   identity when `UPDATE`/`DELETE` are published; PostgreSQL 16+ subscribers
   still use the primary key to apply them). Publication `fb_move_<id>` with
   a row filter per table: `WHERE (_tenant = '<t>')`, `WHERE (tenant =
   '<t>')`, or for parent-class tables a constant list (`WHERE (collection_id
   IN (17, 23))`), which the freeze on schema operations keeps complete.
   Order-defining tables are published into **staging tables** on the target.
   Whole tables owned by the tenant (its pgmq queues) are published without
   a filter.
2. **Copy.** On the target: `CREATE SUBSCRIPTION fb_move_<id> … WITH
   (copy_data = true, origin = none, binary = true, disable_on_error =
   true)`, plus `failover = true` when the source set has replicas
   (PostgreSQL 17+ failover slots let a move survive a source promote).
   PostgreSQL copies only filtered rows, then streams changes. Progress:
   tables ready (`pg_subscription_rel`) and bytes copied
   (`pg_stat_progress_copy` on the source). The target builds the tenant's
   vector indexes concurrently meanwhile.
3. **Catch up** until every table is ready and apply lag is under 1 MiB.
4. **Switch** (the freeze; at most `move_freeze_timeout_ms`, 10 s, else
   abort and unfreeze):
   1. The router freezes the tenant: its new requests wait (they are not
      refused) and background loops skip it; KV flushes the tenant's dirty
      keys.
   2. In-flight transactions of the tenant drain (the router counts them).
   3. A barrier row is written to `_forgebase_move_barrier` on the source
      (published) and awaited on the target: everything before it is applied.
   4. On the target, one transaction renumbers staged rows in source order
      into the real tables with the target's sequences, remaps references
      (subscription cursors, delivery entries, snapshots), and sets the
      sequences of whole moved tables (`setval` to max + 1; PostgreSQL 18
      does not replicate sequences). The source's record-source relay is
      drained up to the barrier first.
   5. Verify per table: row counts always, a SHA-256 over rows in key order
      for tables up to 100 000 rows of the tenant, a 1 % key sample above
      that. A mismatch aborts.
   6. Flip the placement in control (`set_id = to`, `epoch + 1`) in one
      transaction; rewrite `topology.json`.
   7. Unfreeze: waiting requests run on the target. Live realtime
      subscriptions of the tenant get `reset` (`tenant_moved`); stale graph
      materialisation is marked for rebuild.
5. **Cleanup** after `move_source_retention_hours` (24): drop the
   subscription, publication and slot, restore replica identities, delete the
   tenant's rows on the source in batches of 10 000. Until then "Undo move"
   is offered (a move back).
6. **Abort** before the flip drops the subscription and publication,
   deletes copied rows on the target and unfreezes; nothing visible changed.

Logical replication's apply worker is a second writer on the target. It only
writes rows of a tenant that no service sees until the flip, and it fires no
ordinary triggers or outbox rows, so the single-writer invariants of the
services hold.

### 3.9 Adding, draining and removing shards; rebalancing

- **Add** (`database.shard.add`): provision the node (`local`: `initdb` under
  `postgres/nodes/<name>`; `remote`: join token; `external`: URL of an empty
  database), migrate, replay the catalog, set sequence ranges (next free k),
  subscribe to `fb_control`, mark `active`.
- **Drain**: `state = draining` (no new placements), then a rebalance that
  moves every tenant away. **Remove** (empty sets only, typed name): stop the
  nodes; keep data under `postgres/retired/<name>-<time>/` until "Delete
  data" (typed name again).
- **Rebalance**: a plan (greedy: the smallest projects from the most to the
  least loaded set, until the spread of load scores is within 10 % or
  `max_moves` is reached) is shown with sizes and estimated freeze times; the
  operator confirms; moves run one at a time (`max_parallel_moves` = 1).

### 3.10 Row rules and native RLS per shard

- Policies are generated from control's rules (rls-coverage §3.4) and applied
  on every shard inside the two-phase schema transaction (§3.7).
- Claims are set per transaction (`SET LOCAL`), so `enforce` and "run as" work
  on every node, replicas included (`set_config` is allowed in read-only
  transactions).
- `forgebase.acl()` reads the `control_replicated` ACL table on the shard.
- Direct logins are bound to one tenant; the role is created on the tenant's
  shard primary (replicas get it physically). A move recreates the role on the
  target with the same SCRAM verifier. Administrator logins exist on every
  node; `forgebase db psql --node <name>` picks one.

### 3.11 Shards with replicas

Every set, control included, can have replicas; routing, read-your-writes
tokens (`<set>/<lsn>`), lag limits and promote work per set. A move's
subscription runs against the source primary; the target's replicas receive
the data physically.

### 3.12 Limits in v0.3

- One server process per cluster (decision 19).
- At most 8 191 sets (sequence ranges); tested up to 16.
- One move at a time per pair of sets; freeze at most 10 s.
- openCypher graphs are rebuilt after a move.
- No cross-shard queries (§3.6).

## 4. Configuration and admin API

### 4.1 `forgebase.toml`

```toml
[database]
backend = "postgres"              # default in builds with the engine; "sqlite" in -lite builds

[database.postgres.extensions]    # "auto" (default) | "on" | "off"
vector = "auto"                   # pgvector
graph = "auto"                    # Apache AGE (openCypher endpoint)
queue = "auto"                    # pgmq (subscription deliveries)
cron = "auto"                     # pg_cron (schedules)
trigram = "auto"                  # pg_trgm (trigram indexes, fuzzy search)

[database.replicas]
read_routing = "replicas"         # "primary" | "replicas" (used only when a set has replicas)
read_your_writes = true
lsn_wait_ms = 50
max_lag_ms = 2000
max_lag_bytes = 67108864
health_interval_ms = 500
slot_wal_keep_size = "8GB"

[database.sharding]
placement = "least_loaded"        # "least_loaded" | "control" (new tenants stay on control)
project_affinity = true
move_freeze_timeout_ms = 10000
move_source_retention_hours = 24
max_parallel_moves = 1

[[database.nodes]]                # optional: external nodes declared in the file
name = "eu-read-1"
set = "control"                   # "control" or a shard name
role = "replica"
url_env = "FORGEBASE_DB_NODE_EU_READ_1_URL"
weight = 1
```

- All keys go into the FB-91 registry with plain help text. `backend` stays
  `restart_required` and not editable from the dashboard (changing it needs
  `db convert`). Topology itself (nodes, shards, placements) is not file
  configuration; it lives in control tables and changes through the API.
  Nodes from the file are seeded at start and shown as "from forgebase.toml"
  (read-only in the dashboard).
- Environment variables follow the existing pattern
  (`FORGEBASE_DATABASE_REPLICAS_MAX_LAG_MS`, …).

### 4.2 Topology store

Control tables (one migration pair): `_forgebase_db_sets (id, name, kind
control|shard, number k, state, weight, hosts_tenants)`, `_forgebase_db_nodes
(id, set_id, name, kind local|remote|external, role primary|replica,
url_sealed, state, weight, engine_version, system_identifier, timeline)`,
`_forgebase_tenant_placements`, `_forgebase_db_moves`,
`_forgebase_xact_journal`, `_forgebase_join_tokens`. The server also writes
`<data_dir>/topology.json` (sets, nodes, epoch) after every change, so a
restart finds the control primary even after a promote.

### 4.3 `/api/system/database/*`

Administrators only (`require_admin`), MFA step-up for promote, remove and
move when MFA is on, every call audit-logged. Long operations answer `202
{operation}`.

| Method and path | Does |
|---|---|
| `GET /api/system/database/topology` | sets, nodes, roles, health, lag, tenant counts, epoch, running operations |
| `GET /api/system/database/nodes/{id}` | one node with lag history (last hour, one-minute buckets) |
| `POST /api/system/database/nodes` | add a replica `{set, name, kind, url?}` (job) |
| `DELETE /api/system/database/nodes/{id}?confirm=<name>` | remove a replica (drops its slot) |
| `POST /api/system/database/nodes/{id}/promote` | `{confirm, accept_data_loss?}` (job) |
| `POST /api/system/database/nodes/{id}/rebuild` | re-clone or rejoin with `pg_rewind` (job) |
| `POST /api/system/database/join-tokens` | `{set, role}` → `{token, expires_at, command}` for `forgebase node join` |
| `POST /api/system/database/shards` | add a shard `{name, node, weight}` (job) |
| `PATCH /api/system/database/shards/{id}` | `{state: active|draining, weight, hosts_tenants}` |
| `DELETE /api/system/database/shards/{id}?confirm=<name>` | remove an empty shard (`409 shard_not_empty`) |
| `GET /api/system/database/placements` | tenants with set, project, size estimate, state; filters `set`, `project`, `q`; cursor pages |
| `POST /api/system/database/placements/{tenant}/move` | `{to, confirm}` (job) |
| `POST /api/system/database/rebalance/plan` | `{sets?, max_moves?}` → plan with moves, sizes, freeze estimates |
| `POST /api/system/database/rebalance` | `{plan_id, confirm}` (job) |
| `GET /api/system/database/operations`, `GET …/operations/{id}`, `POST …/operations/{id}/cancel` | topology operations (a filtered view of jobs) |

- Errors: `409 operation_in_progress` (one topology change per set at a
  time), `409 tenant_moving`, `409 shard_not_empty`, `400 node_mismatch`,
  `400 sharding_unsupported`, `503 node_unreachable`; SQLite answers `400
  postgres_required` (and `GET …/topology` shows one SQLite node).
- **Operations are jobs** of reserved kinds (FB-142's reserved list):
  `database.node.add`, `database.node.rebuild`, `database.promote`,
  `database.shard.add`, `database.shard.drain`, `database.tenant.move`,
  `database.rebalance`, plus the engine jobs `vector.convert`,
  `queue.convert`, `graph.materialise`. They live on control, report progress
  (`percent`, `stage`, `message`) on channel `jobs:<id>` (ADR-0023 §3),
  checkpoint their state so a restart resumes them, and honour cancel at safe
  points (a move cancels into `aborted` before the flip only).

## 5. Dashboard: Database topology screen

**Where**: System studio (Operate group), tab "Database topology". The
Database studio (Build group) is about data: collections, SQL, indexes.
Topology is operations (servers, failover, capacity), which is what Operate
holds in `docs/requirements/dashboard-ux.md` rule 7. FB-155's backend panel in
the Database studio links here ("View topology").

Screen spec (every rule of `dashboard-ux.md` applies):

- **Header**: "Database topology", "Where your data lives: the primary
  server, its read replicas and your shards, with their health." Docs link.
  Primary action "Add replica" with "Add shard" in its menu.
- **Summary strip**: backend and engine version; servers healthy / total;
  highest replica lag; shards; tenants; last operation with its state.
- **Sets**: one card per set (Control, Shard 1, …). Each node row: name; kind
  ("This server", "Other host", "External"); role; state chip (Streaming,
  Catching up, Lagging, Unreachable, Needs rebuild); lag as time and bytes
  with a one-hour sparkline; connections; disk free; engine version. Row menu:
  Promote (replicas), Rebuild, Remove. Card menu: Add replica, Drain, Set
  weight, Remove shard.
- **Tenants**: table of environment, project, organization, set, estimated
  size, state; filters by set and project; search. Row action "Move to…":
  dialog with the target set, copy size, estimated freeze time, confirm.
- **Rebalance**: "Plan rebalance" shows the plan table (tenant, from, to,
  size, freeze estimate); "Run plan" confirms.
- **Operations**: live list (stage, percent bar, message), cancel where
  allowed, link to the job.
- **Confirmations**: Promote explains what happens, shows the data-loss
  estimate when the primary is unreachable and asks for the node name;
  removing a replica or shard asks for its name; Move and Rebalance confirm
  with the freeze estimate; Drain confirms.
- **Empty state** (one server): says in plain words what a read replica and a
  shard are and when to add one, with both actions, and "Nothing changes
  until you add one."
- **SQLite projects**: "Replicas and shards need PostgreSQL", with the copyable
  `forgebase db convert --to postgres` command.
- **Help tips** for replica, lag, shard, promote, rebalance, freeze.
- **Live**: topology polled every 5 s while visible; operations follow their
  `jobs:<id>` channel. Errors use the shared error panel (message, next step,
  request id).
- **Use from code**: a panel with the SDK `consistency: "strong"` option and
  the `X-ForgeBase-Read-After` header.
- **Tests**: unit tests for each dialog's confirmation, the empty and SQLite
  states, and an axe check; the journey walkthrough gains "add a local replica,
  see it streaming".

## 6. PostgreSQL becomes the default backend

- **Default**: `[database] backend` defaults to `postgres` (managed) in builds
  with `bundled-postgres`; `-lite` builds keep `sqlite` (a compile-time
  default) and say so in `forgebase --version`.
- **`forgebase init`**: `--database postgres` is the default; the terminal
  prompt lists "[1] PostgreSQL (default, bundled server) [2] SQLite (one
  file)". As root, managed mode is refused (ADR-0028 §4.2) with "PostgreSQL
  cannot run as root: run as an unprivileged user, or use --database sqlite".
  No silent fallback.
- **`forgebase serve`**: the backend marker wins (ADR-0028 §6.3). No marker
  and an existing `forgebase.db` → SQLite (existing projects never switch on
  their own). An empty data directory → the configured default.
- **Existing SQLite projects** keep working. `forgebase doctor`, `forgebase
  status` and the System studio show a one-line notice with `forgebase db
  convert --to postgres` (FB-151). Release notes get "Upgrading to 0.3".
- **Docker**: the image initialises PostgreSQL by default (it already runs as
  the `forgebase` user); the compose example keeps the `shm_size` note.
- **Tests**: unit tests keep `FORGEBASE_TEST_BACKEND` defaulting to SQLite
  (speed); CI runs both (FB-149); the black-box suites test the new default
  `init` and a `--database sqlite` run.
- **Docs**: README quick start, `docs/database.md` (backends, extensions,
  replicas, sharding), `docs/deployment.md`, `forgebase.example.toml`, the
  dashboard's setup copy.

## 7. Rust changes by crate

| Crate | Change | Package |
|---|---|---|
| `forgebase-db` | `pg/extensions.rs`; `routing/` (sets, node pools, monitor, LSN capture, `fetch_routed`, `ReadIntent`); `placement.rs` (registry, classes, completeness test); `DbRouter`, `SetHandle`; sequence ranges; two-phase coordinator; `topology.json` | FB-161, FB-167, FB-170, FB-173 |
| `forgebase-pgengine` | extension settings; standby provisioning, slots, promote, `pg_rewind`, replicator role; `hba` replication entries | FB-132 scope, FB-168 |
| `forgebase-cli` | `forgebase node join`; `db psql --node`; `init` default | FB-168, FB-177 |
| `forgebase-migrations` | runner over all nodes; topology tables migration pair | FB-170, FB-173 |
| `forgebase-data` | `AccessContext.read_after`; routed reads; per-call set resolution; cross-placement relation rules; schema fan-out hook; `trigram`, `fuzzy` | FB-166, FB-169, FB-171, FB-173 |
| `forgebase-query` | `plainto_tsquery`, trigram operators; relation-crossing check | FB-166, FB-171 |
| `forgebase-policy` | native policies applied per node; `control_replicated` publication | FB-173 |
| `forgebase-realtime` | dispatcher and pruner per set; commit-LSN sampling; `tenant_moved` reset | FB-169, FB-171 |
| `forgebase-functions` | after-hook cursor per set; context tenant for routing | FB-171 |
| `forgebase-jobs` | pg_cron; claim loop per set; reserved `database.*` kinds | FB-165, FB-172, FB-175 |
| `forgebase-queue` | pgmq deliveries; per-set maintenance; relay cursors per set on control | FB-164, FB-172 |
| `forgebase-kv` | flusher grouped by set; load from every set | FB-172 |
| `forgebase-vector`, `forgebase-graph` | pgvector store; AGE derived graph; per-tenant set resolution | FB-162, FB-163, FB-172 |
| `forgebase-storage` | per-tenant set resolution (after G14) | FB-172 |
| `forgebase-api` | `/api/system/database/*`; commit and read-after headers; consistency header | FB-167, FB-175 |
| `forgebase-server` | builds `DbRouter` instead of one `Database`; starts per-set loops | FB-171 |
| `forgebase-backup` | per-set backups, replica source, `needs_rebuild` after restore | FB-179 |
| `sdk/js`, `sdk/rust` | commit token tracking, `consistency` option | FB-167 |
| `dashboard` | Database topology screen | FB-176 |

## 8. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| AGE build on musl and macOS fails, or AGE lags PostgreSQL 19 | openCypher missing on some targets | AGE is isolated in FB-160 and derived (§1.5): the REST graph never depends on it; a target without AGE only loses openCypher |
| AGE's RLS gaps in traversals | rules bypassed inside Cypher | v0.3 does not rely on AGE RLS: per-tenant graphs, read-only, refused for callers with label rules (open question 1) |
| Sharding and replicas lengthen the v0.3 critical path by about three waves | later v0.3 | packages split by crate; extensions and replicas do not wait for sharding; open question 3 |
| Two-phase commit leaves prepared transactions holding locks | stuck DDL on a shard | journal and recovery at start and every minute; `doctor` lists in-doubt transactions; DDL timeout |
| Freeze exceeds 10 s on big or busy tenants | requests wait | abort and unfreeze automatically; the plan shows freeze estimates; large tenants move off-peak |
| A missed table in the placement registry | data left behind on a move | completeness test against the schema; move verification counts every registered table |
| Replica lag surprises (stale reads) | users see old data | opt-in call sites only; commit tokens; lag limits; `strong` consistency option |
| Logical decoding (`wal_level = logical`) adds WAL volume | more disk and I/O | small for row changes; documented; measured in FB-178 |
| pg_cron or logical apply act as second writers | broken single-writer invariants | limited to fixed functions and invisible tenants; FB-146's test pins exactly what is allowed |
| pgmq internals change between versions | broken claims | pinned version, layout check at start, portable fallback |

## 9a. Human decisions (2026-11-11)

These override the recommendations in section 9 where they differ.

1. **openCypher: full rules now (option B).**
   - FB-160 carries the three unreleased AGE fixes (#2475, #2561, #2578) as
     pinned patches in the engine build. Each patch has its upstream link and
     checksum, and is dropped when AGE releases it.
   - FB-163 enforces label rules inside Cypher. A caller with label rules is no
     longer refused: their rules apply to every node and edge in the traversal.
   - Cypher stays read-only and per tenant, and needs `engine:graph_cypher`.
   - FB-163 adds differential tests: Cypher results match the REST graph API
     under the same rules.
2. **Cross-shard links: keep them and limit rules (option A).** This is as
   recommended.
3. **Release gate: hold v0.3.0 for everything (option B).** FB-174 (online
   moves and rebalance) and FB-178 (topology CI and chaos tests) are in
   v0.3.0. There is no v0.3.1 split.

## 9. Open questions for the human

1. **openCypher and graph rules.** AGE 1.8.0 (PG18) skips row-level security
   inside variable-length and shortest-path traversals; the fixes are on AGE's
   `master` only.
   - **A (recommended):** v0.3 openCypher is read-only, needs the
     `engine:graph_cypher` permission, isolates tenants structurally (one
     derived graph per tenant), and is refused for callers to whom a graph
     label rule applies. Label rules inside Cypher come with the next AGE
     release.
   - B: carry the three unreleased AGE fixes (#2475, #2561, #2578) as reviewed
     patches on 1.8.0, and compile label rules to RLS on AGE's label tables in
     v0.3 (more build and security work, unreleased upstream code).
2. **Relations from tenant data to global collections on a sharded cluster**
   (for example `posts.author → users`).
   - **A (recommended):** keep them working without an SQL foreign key
     (existence checks by the data core, two-step `expand`, `ON DELETE`
     fan-out jobs), and refuse rules and filters that follow such a relation,
     listing them before the first extra shard is added.
   - B: replicate global collections read-only to every shard so rules can
     follow relations everywhere (v0.4: adds replication lag to rule
     results).
3. **Release gate for sharding.** Sharding adds about two waves to the
   critical path, and online moves are the riskiest part.
   - **A (recommended):** v0.3.0 ships extensions, replicas, the default
     switch and sharding with placement of new tenants, adding shards and
     routing (FB-160 to FB-173, FB-175 to FB-177, FB-179); online moves and
     rebalance (FB-174) and the move chaos tests (FB-178) ship in v0.3.1
     after a soak.
   - B: hold v0.3.0 until every package FB-160 to FB-179 is merged.

## Consequences

- PostgreSQL becomes the default; SQLite stays complete and is the `-lite`
  build. Every feature keeps one API on both backends; PostgreSQL adds
  openCypher, trigram indexes and fuzzy search, replicas and shards.
- Each engine gains a second storage path on PostgreSQL, so both paths are
  tested in CI (accelerated and portable).
- ForgeBase now runs and supervises several PostgreSQL servers and moves data
  between them, which raises the operational surface; the topology screen and
  jobs with progress make it visible.
- Automatic failover and several server processes are explicitly v0.4.

## Sources

Checked 2026-11-11.

- pgvector licence, changelog (0.8.7, PG18 from 0.8.1), CI, Windows and
  portability notes — <https://github.com/pgvector/pgvector/blob/master/LICENSE>,
  <https://github.com/pgvector/pgvector/blob/master/CHANGELOG.md>,
  <https://github.com/pgvector/pgvector/blob/master/.github/workflows/build.yml>,
  <https://github.com/pgvector/pgvector#windows>
- Apache AGE licence, PG18 releases and vote, RLS and fixes —
  <https://github.com/apache/age/blob/PG18/LICENSE>,
  <https://github.com/apache/age/releases/tag/PG18%2Fv1.8.0-rc0>,
  <https://lists.apache.org/thread/g9r6z3q13yobzfwmbv9vgf98ho5xf925>,
  <https://dist.apache.org/repos/dist/release/age/PG18/1.8.0/>,
  <https://github.com/apache/age/releases/tag/PG18%2Fv1.7.0-rc0>,
  <https://github.com/apache/age/pull/2309>, <https://github.com/apache/age/pull/2475>,
  <https://github.com/apache/age/pull/2561>, <https://github.com/apache/age/pull/2438>,
  <https://github.com/apache/age/pull/2578>, <https://github.com/apache/age/pull/2345>,
  <https://github.com/apache/age/issues/2502>, <https://github.com/apache/age/blob/PG18/Makefile>
- pgmq licence and releases (1.13.1; pure SQL since 1.3.2; PG18 since 1.8.0) —
  <https://github.com/pgmq/pgmq/blob/main/LICENSE>,
  <https://github.com/pgmq/pgmq/releases/tag/v1.13.1>,
  <https://github.com/pgmq/pgmq/releases/tag/v1.3.2>,
  <https://github.com/pgmq/pgmq/releases/tag/v1.8.0>,
  <https://github.com/pgmq/pgmq/blob/main/INSTALLATION.md>
- pg_cron licence, releases (1.6.8 with Windows; PG18 since 1.6.6), standby
  behaviour — <https://github.com/citusdata/pg_cron/blob/main/LICENSE>,
  <https://github.com/citusdata/pg_cron/releases/tag/v1.6.8>,
  <https://github.com/citusdata/pg_cron/pull/431>,
  <https://github.com/citusdata/pg_cron/releases/tag/v1.6.6>,
  <https://github.com/citusdata/pg_cron#setting-up-pg_cron>
- pg_trgm and full-text search — <https://www.postgresql.org/docs/18/pgtrgm.html>,
  <https://www.postgresql.org/docs/18/textsearch.html>
- LSN waits: revert of `pg_wal_replay_wait` —
  <https://github.com/postgres/postgres/commit/3a7ae6b3d91e0d011dba1eb8a29e1836c6a33c75>;
  PostgreSQL 19 `WAIT FOR` — <https://www.postgresql.org/docs/19/sql-wait.html>;
  WAL functions — <https://www.postgresql.org/docs/18/functions-admin.html>
- Logical replication row filters, restrictions (no sequences in 18), PG19
  sequence sync — <https://www.postgresql.org/docs/18/logical-replication-row-filter.html>,
  <https://www.postgresql.org/docs/18/logical-replication-restrictions.html>,
  <https://www.postgresql.org/docs/19/release-19.html>,
  <https://www.postgresql.org/docs/18/release-18.html>
- `pg_rewind` — <https://www.postgresql.org/docs/18/app-pgrewind.html>;
  failover slots — <https://www.postgresql.org/docs/17/release-17.html>;
  `idle_replication_slot_timeout` —
  <https://www.postgresql.org/docs/18/runtime-config-replication.html>
- Citus licence (AGPL-3.0) — <https://github.com/citusdata/citus/blob/main/LICENSE>

## Addendum: FB-170, the sharding core in `forgebase-db` (2026-11-11)

Built as §3.1–§3.5 and §4.2 describe. Nothing outside `forgebase-db` uses
the router yet; the packages that wire it are listed at the end. Where the
code differs from the text above, or decides something it left open, it is
said here.

**Files.** `crates/forgebase-db/src/{topology.rs, placement.rs, router.rs}`
(new), the migration pair `migrations/{system,postgres}/0033_db_topology.sql`
(registered in `SYSTEM_MIGRATIONS` and `POSTGRES_MIGRATIONS`), module and
re-export lines in `crates/forgebase-db/src/lib.rs`, and the tests
`crates/forgebase-db/tests/sharding.rs`.

**Topology tables (0033).** The six tables of §4.2, all placement class
`control`, with the columns §4.2 names plus:
- `_forgebase_db_sets`: `number` 0 … 8 190 (8 191 sets, §3.12), unique;
  `CHECK ((kind = 'control') = (number = 0))`, so exactly one control set.
  States `provisioning | active | draining | migrating | retired`. The
  migration seeds the control set (`id = name = 'control'`, number 0,
  active, weight 1, hosts tenants) on both backends.
- `_forgebase_db_nodes`: states of §2.7 plus `ready` (a primary that
  answers); `origin api | config` (nodes from `[[database.nodes]]` are
  read-only in the dashboard, §4.1); at most one non-retired primary per set
  (partial unique index). Control's primary may have no row: it is the
  database the server opened.
- `_forgebase_tenant_placements`: `est_rows`, `est_bytes`, `stats_at`
  (the hourly statistics of §3.4, read by the load score and plans). No
  foreign key to `_forgebase_environments`: the row must outlive a deleted
  environment until its data on the shard is gone.
- `_forgebase_db_moves`: `state` as §3.8, `job_id`, `rebalance_id`,
  `detail` (JSON checkpoint), `error`, `switched_at`, `finished_at`; one
  open move per tenant (partial unique index).
- `_forgebase_xact_journal`: `gid` (≤ 180 characters, so `<gid>_<set
  number>` fits PostgreSQL's 200-byte prepared-transaction names), `nodes`
  (JSON array of set ids), `state committing | committed`, `purpose`,
  `resolved_at`. No `aborted` state: a failure before the row exists rolls
  every part back, so recovery rolls back in-doubt transactions without a
  row and commits those with a `committing` row.
- `_forgebase_join_tokens`: `token_hash` (SHA-256 hex; the token is shown
  once), `set_id`, `role`, `expires_at`, `used_at`, `node_id`.
- Times are RFC 3339 text, as elsewhere.

**Topology model (`topology.rs`).** Plain data with `serde` on every type
(FB-175 and FB-176 show and edit it): `DbSet`, `DbNode`, `Placement`,
`Topology { epoch, sets, nodes }`, and the stored enums `SetKind`,
`SetState`, `NodeKind`, `NodeRole`, `NodeState`, `NodeOrigin`,
`PlacementState`, `MoveState`, `XactState` (snake-case text; a unit test
checks every value against both migrations' `CHECK`s). The **topology
epoch** is one counter on control (`_forgebase_meta` key `topology_epoch`),
bumped in the same transaction as every set, node or placement change; each
placement row also has its own `epoch` (+1 on every change of the row).
Store functions (control only): `load`, `load_placements`,
`load_placement`, `insert_set` (next unused number, never reused: rows
allocated from a retired set's range may exist elsewhere; state
`provisioning`; PostgreSQL only), `update_set`, `insert_node`,
`update_node_state`, `upsert_placement`, `record_placement_stats` (no epoch
change), `bump_epoch`.

**`topology.json`.** `write_topology_file` / `read_topology_file`:
`{ version: 1, written_at, epoch, sets, nodes }`, written atomically
(temporary file renamed over it, mode 0600 on Unix). Nodes carry
`url_sealed` (ciphertext, sealed with the instance keyring) so a restart
finds the control primary after a promote; placements are not in the file
(there can be very many). The router writes it after every change it sees,
only when a path is configured.

**Sequence ranges (§3.3).** `sequence_range(k) = [k·2⁴⁰ + 1, (k+1)·2⁴⁰ −
1]`, `set_number_of(id) = id >> 40`. `apply_sequence_range(db, k)` sets
`MINVALUE`, `MAXVALUE` and `START` of every sequence in schemas `forgebase`
and `public` (18 identity sequences at head) in one transaction; a sequence
whose position is inside the range keeps it, one outside restarts at the
start. Idempotent. On control (k = 0) only `MAXVALUE` changes and ids
continue (tested). SQLite: k = 0 is a no-op, other numbers are
`Unsupported`.

**Placement registry (`placement.rs`).** `TABLES` lists all 154 system
tables at head with `PlacementClass` (`control`, `tenant`, `per_set`,
`control_replicated`), `TenantKey` (`Column`, `Parent { column, table,
parent_column }`, or `Record { collection_column, record_column }` for
record versions) and `MoveStrategy` (`stay`, `copy`, `renumber`, `remap`,
`rebuild`). `collection_placement(tenant_scoped)` classifies user
collection tables (`_tenant`, else control). `system_tables(db)` reads the
schema (`forgebase` on PostgreSQL; `_forgebase_*` on SQLite, FTS5 shadow
tables left out, a virtual table's hidden `rowid` included) and
`problems(schema)` returns every gap: an unclassified or stale table, a
missing key column, a parent that is not a tenant table, or a `tenant` /
`_tenant` column that is neither the key nor in
`TENANT_COLUMNS_NOT_KEYS` (with a reason). The tests run it on SQLite and
PostgreSQL and also check both backends list the same tables. Choices the
ADR left open:
- `per_set`: `_forgebase_meta`, `_forgebase_migrations`,
  `_forgebase_realtime_outbox`, `_forgebase_function_cursors`,
  `_forgebase_queue_state` (relay cursors; a record-source topic on control
  keeps one key per set there, §3.5), `_forgebase_queue_rates` (rate
  windows restart on the target), `_forgebase_job_queues` (the claim SQL on
  each set reads its own settings and windows; FB-172 keeps the settings in
  step with control), `_forgebase_storage_blobs` (`rebuild`: content-
  addressed reference counts are recounted on the target).
- `control`: `_forgebase_job_workers` (HTTP workers register once and claim
  round-robin), `_forgebase_storage_settings`, every audit log. API keys
  and the authorization audit keep their `tenant` column as data.
- `tenant` by column: KV, graph nodes and edges, vector collections, storage
  buckets, jobs, schedules and workflows (`0030_job_tenant`), queue topics,
  subscriptions (`remap`) and snapshots (`remap`) (`0031_queue_rls`). By
  parent: vector fields, items, values and `_forgebase_vector_fts` (via the
  item's `key`; logical apply fires no triggers, so the keyword rows are
  copied), job events and attempts (`renumber`), job deps, queue messages
  (`renumber`), deliveries and snapshot items (`remap`), dedup and dead
  letters, storage objects, ACL, uploads and parts.
  `_forgebase_record_versions` by record. `PENDING_TENANT_COLUMNS` (a key
  column a package in flight adds, accepted while missing) is empty.
- `control_replicated`: `_forgebase_resource_permissions`.

**Router (`router.rs`).** `DbRouter::single(db)` (one fixed set, reads and
writes nothing) and `DbRouter::open(control, RouterOptions { connector,
topology_file, policy, freeze_timeout })` (reads topology and placements;
opens every routable set other than control through a `SetConnector`, which
the server implements by unsealing the node URL). `control()`,
`for_tenant(TenantRef) -> SetHandle`, `try_for_tenant`, `sets()` (control
first), `set(id)`, `set_by_number(k)`, `is_sharded()`, `epoch()`,
`topology()`, `placement(t)`, `placements()`, `subscribe()` (epoch on a
`watch` channel), `reload()`, `place(t, set, state)`,
`place_new_tenant(t, project_id, requested)`, `apply_placement`,
`set_loads()`; `least_loaded(&[SetLoad])`. `SetHandle` implements `Backend`
on the set's primary and derefs to its `Database`. Routing rules:
- No tenant, `""`, or a tenant without a placement row: control. A
  placement on a set that is not open: `RouterError::SetUnavailable`, never
  a silent fallback.
- `frozen`: `for_tenant` waits for the next placement change, at most
  `freeze_timeout` (10 s), then `RouterError::TenantMoving`;
  `try_for_tenant` refuses at once (for background loops). `moving` routes
  to the source, `cleanup` to the new set.
- The cache only takes a row with a higher epoch than the one it holds, so
  a reload that read before a concurrent `place` cannot undo it.
- **One set behaves as today**: every call returns the one `Database`;
  `place_new_tenant` writes no row (a tenant without a row is on control);
  `open` reads the topology once and writes `topology.json` only when a
  path is given. Tested on both backends.
- New tenants (§3.4), once sharded: an existing placement is kept; an
  administrator's `requested` set must be `active`; otherwise project
  affinity (the set of the project's earliest other environment in
  `_forgebase_environments`, if it takes tenants), then policy mode
  `control`, or the least-loaded set taking tenants: estimated bytes ÷
  weight, then placed tenants ÷ weight, then the lower number. A placement
  row is written for every new tenant, control included.
- Errors are `RouterError` (`ErrorCode`): `shard_not_found` (404),
  `shard_unavailable` (503), `shard_closed`, `sharding_unsupported`,
  `postgres_required` (400), `tenant_moving` (409),
  `shard_connector_missing` (500), or the `DbError`. `DbError` is
  unchanged.

**Deviations.**
- Migration number 0033 (god assigned it: FB-141 took 0031, FB-137 takes 0032).
- `for_tenant` is `async` (it waits during a freeze); §3.5 does not say.
- On PostgreSQL an **uncached** read (`Statement::raw`) whose result has a
  `citext` column fails on a fresh read connection with "unnamed prepared
  statement does not exist" (the second try works): FB-131's backend,
  outside this package, not fixed here. The topology reads use cached
  statements. FB-143 (SQL editor reads are uncached) should look at it.
- Shared edit outside Owns: `crates/forgebase-db/src/sqlite.rs` test helper
  gained `..Config::defaults(..).database` (main's `forgebase-db` unit tests
  did not compile after FB-148 added `DatabaseConfig` fields).

**For the next packages.**
- **FB-167 (replica reads):** put the node pools and `fetch_routed` behind
  `SetHandle` (today `{ set, db: Database }`, primary only); the set's
  replicas are `Topology::nodes_of(set)` with role `replica`; health states
  go through `topology::update_node_state` (bumps the epoch, so call
  `reload` or keep health in memory and persist only transitions).
  Read-your-writes tokens are `<set id>/<lsn>`.
- **FB-171 (server, data core, realtime, functions):** build the router
  with `DbRouter::open(control, RouterOptions { connector, topology_file:
  Some(<data_dir>/topology.json), policy from [database.sharding],
  freeze_timeout from move_freeze_timeout_ms })` after `migrate`, and pass
  it instead of the one `Database`; implement `SetConnector` (unseal
  `url_sealed`, open with the server's `DbOptions`). Route with
  `for_tenant(TenantRef::new(ctx.tenant.as_deref()))`. Call
  `place_new_tenant(env_id, Some(project_id), shard)` when an environment
  is created. `tenant_required_sharded` uses `is_sharded()`. Realtime per
  set: `sets()`; old cursors → `topology::set_number_of(cursor)`.
  `_forgebase_record_versions` has no tenant column: write versions on the
  record's set (its key is the record).
- **FB-172 (jobs, queues, KV, vector, storage):** one loop per
  `sets()` entry; skip tenants whose `placement(t)` is `moving` or `frozen`
  (`try_for_tenant` returns `TenantMoving` for frozen ones);
  `_forgebase_job_queues` is per set, keep its settings in step.
- **FB-173 (fan-out, migrations):** `xact_journal` as described above;
  `insert_set` then migrate, replay the catalog, `apply_sequence_range(db,
  set.number)`, then `update_set(.., Active)` and `reload()`. Run
  `apply_sequence_range(control, 0)` once when the first shard is added.
- **FB-174 (moves, rebalance):** `TABLES` gives the row filter per table
  (`tenant_column()`, `Parent`, `Record`) and the move strategy; moves are
  rows in `_forgebase_db_moves`; the switch is `place(t, from, Frozen)` →
  … → `place(t, to, Cleanup)` (epoch + 1 each, `moved_at` set when the set
  changes) → `place(t, to, Active)` after cleanup. `set_loads()` and
  `least_loaded` give the plan's scores. Before the first shard, write a
  control placement row for every existing environment so control's load
  is counted (tenants without a row have no size).
- **FB-176 (dashboard):** every topology type serialises as stored (snake
  case); show `origin = config` nodes read-only; never show `url_sealed`
  (FB-175's API must leave it out).
- **FB-153:** add `_forgebase_db_logins` to `TABLES` as
  `control_replicated`. **FB-164:** pgmq queues are tenant-owned whole
  tables outside the registry; their `msg_id` sequences live in schema
  `pgmq`, which `apply_sequence_range` does not touch.

## Addendum: FB-161, the extension layer (2026-11-11)

Built as §1.1–§1.3 describe. Where the code differs from the text above, or
decides something it left open, it is said here.

**Files.** `crates/forgebase-db/src/pg/extensions.rs` and
`crates/forgebase-pgengine/src/extensions.rs` (new); module and re-export
lines in both crates; `DatabaseInfo.features`; the setup call in
`forgebase-server`'s start (after `migrate`, before anything else touches
the database); the `features` field of `/api/system/info`
(`forgebase-api/src/routes.rs`). Tests:
`crates/forgebase-db/tests/{extensions.rs, pg_uncached_reads.rs}`,
`crates/forgebase-pgengine/tests/extensions.rs`, unit tests in both new
modules, and the FB-148 system-info test in `forgebase-api`. No migration.

**API (`forgebase-db`).**
- `Feature` (`Vector`, `Graph`, `Queue`, `Cron`, `Trigram`): `name()`
  (the `[database.postgres.extensions]` key), `extension()`, `label()`,
  `stateful()`, `needs_superuser()`, `target_schema()`, `mode(&modes)`.
- `Database::setup_extensions(&ExtensionSetup { modes, admin_url,
  reader_role })` → `ExtensionReport` (one `FeatureStatus` per feature:
  `engine` = `"pgvector 0.8.7"` or `"portable"`, `state` = `active` |
  `portable` | `off`, `mode`, `extension`, `available`, `installed`,
  `schema`, `schema_version`, `reason`). The report is kept on the
  handle: `Database::extensions()`, `Database::feature_active(Feature)`
  (no query; `false` on SQLite and until the setup ran) and
  `Database::info().features`.
- `Database::detect_extensions(&modes)`: the same decisions without
  creating, granting or recording anything (one-off commands, tools; the
  API falls back to it when the process did not run the setup).
- Errors: `ExtensionError::Refused(Vec<String>)` (code
  `extension_unavailable`, all problems in one message) or `Db`. The server
  maps it to `ServerError::Extensions`; the start fails.

**Detection.** One read of `pg_available_extensions` full-joined with
`pg_extension` (so an installed extension whose files are gone still
shows), plus `shared_preload_libraries`, `session_preload_libraries` and
`cron.database_name` read from `pg_settings` (superuser-only settings are
hidden there, not an error), whether any `age.*` setting exists,
`current_database()`, `current_user` and whether it has
`pg_read_all_settings`. **The preload lists are superuser-only settings**:
the bundled engine grants `pg_read_all_settings` to `forgebase_server` on
every start. A feature is usable when its control file exists or it is
installed, and:
- pg_cron: `pg_cron` is in `shared_preload_libraries` and
  `cron.database_name` is this database; a role that cannot read the
  settings accepts an installed pg_cron (installing it needs both);
- AGE: `age` is in this session's `session_preload_libraries` (or
  `shared_preload_libraries`), or one of AGE's `age.*` settings exists.

**Decision (decided here; §1.1 left it open).** Vector and queue are
**stateful**: they keep canonical data in the extension's storage. The
first setup records their engine in `_forgebase_meta`
`ext.<feature>.engine = extension | portable`, and later starts follow it:
- `auto` with `portable` stays portable even when the extension appears
  (reason names `<feature>.convert`); `on` with `portable` refuses;
- `extension` must stay usable in every mode: missing extension refuses,
  `off` refuses (run `<feature>.convert` first).
Graph, cron and trigram hold only derived state (the rebuilt AGE graph,
`cron.job` rows synced from schedules, an index kind), so they follow their
mode on every start with no record. `auto` without a usable extension is
portable with a `reason`; `on` refuses; `off` is portable and creates
nothing.

**Creation and owners.** `CREATE EXTENSION IF NOT EXISTS`, pgvector and
pg_trgm `WITH SCHEMA forgebase` (next to the baseline's `citext` and
`unaccent`); pgmq, pg_cron and AGE have fixed schemas (`pgmq`, `cron`,
`ag_catalog`). pgvector, pg_cron and AGE need a superuser: created through
`admin_url` (`forgebase_super` on the bundled engine; without it, the
database's own role, as an external server's configured role). **pgmq and
pg_trgm are created by the server role itself**, which then owns them:
pgmq's queue tables are created by `pgmq.create` as the caller.
A failed `CREATE EXTENSION` counts as "not usable" (auto falls back with
the error as reason).

**Grants (every start, idempotent; managed passes `reader_role =
forgebase_reader`).** pgmq: the server gets `USAGE, CREATE` on the schema
and rights on its tables, sequences and functions; the reader `USAGE`,
`SELECT` on all tables and default `SELECT` on tables the server creates
there. pg_cron: `USAGE ON SCHEMA cron` to the server (`cron.schedule` is
`PUBLIC`; `cron.job` rows are per user by pg_cron's own RLS). AGE: `USAGE ON
SCHEMA ag_catalog` to both, `SELECT` on its tables to the reader. pgvector
and pg_trgm live in `forgebase`, which both roles already use.

**Versioned DDL.** `EXTENSION_DDL` lists `(feature, version, sql)`; `{schema}`
is replaced by the extension's schema. Pending steps run in version order
on the writer, each in one `SchemaChange` transaction that also writes
`ext.<feature>.schema = N`. Version 1 of vector is §1.4's column:
`ALTER TABLE _forgebase_vector_values ADD COLUMN IF NOT EXISTS vec
forgebase.vector, ALTER COLUMN vector DROP NOT NULL`. No other feature has
DDL yet. A unit test checks versions count 1, 2, … per feature.

**Engine side (`forgebase-pgengine`).** `EngineConfig.graph`
(`[database.postgres.extensions] graph`, mapped by the server like `cron`).
After the bootstrap of a primary, as `forgebase_super`: `GRANT
pg_read_all_settings TO forgebase_server`, and `ALTER ROLE
forgebase_server|forgebase_reader SET session_preload_libraries =
'$libdir/plugins/age'` when the engine has `lib/plugins/age.*` and graph is
not `off`, otherwise `RESET` (a stale setting naming a missing library would
make every login fail). `graph = "on"` with an engine without AGE is refused
before `initdb`, like `cron = "on"`; `auto` adds a start-up warning.
`EngineDir::has_plugin`, `EngineHandle::has_age`, `AGE_PRELOAD`. The roles'
`search_path` stays `forgebase, public`: FB-163 sets `ag_catalog` with `SET
LOCAL`.

**`/api/system/info`.** `database.features.<feature>` = `{engine, state,
mode, extension, available, installed, schema, schema_version, reason}`.
**Changed from FB-148:** `engine` is real now, and `state` is `active |
portable | off` (FB-148 reported `installed | available | unavailable |
off` with `engine` always `portable`). The dashboard reads only `engine`
and `state` as text.

**Bug fixed (found by FB-170, FB-131's backend).** On PostgreSQL the first
uncached read (`Statement::raw`) whose result had a type sqlx had not seen
on that connection (`citext`, an enum, any extension type such as `vector`)
failed with "unnamed prepared statement does not exist": sqlx 0.9 looks
unknown types up with a simple query between `Parse` and `Bind`, and a
simple query destroys the unnamed statement. Reads now retry once (the type
is cached by then); in a write transaction such statements (uncached, no
parameters) run under a savepoint and are retried after rolling back to it.
Tests fail before the fix: `tests/pg_uncached_reads.rs` (citext on fresh
readers, an enum in a write transaction and on readers).

**Checked end to end.** FB-148's `#[ignore]`d
`the_server_runs_on_the_bundled_engine` (`forgebase-server/tests/database_fb148.rs`)
passes on this branch with FB-130's engine: the server starts the engine,
migrates, sets up the extensions and reports five features. It can be
un-ignored (FB-148's file, not changed here).

**Deviations.**
- §1.1 says "`on` (refuse to start without it)"; for the stateful features
  the stored engine also refuses `off` and `on`-against-portable, so a
  switch is always an explicit conversion.
- Databases set up after this package but before FB-162 / FB-164 record
  `ext.vector.engine = extension` (and queue) while the services still
  write portably; only development databases can be in that state (v0.3 is
  not released): re-create them, or let FB-162/164's conversion handle it.
- Shared edits outside Owns: `forgebase-server/src/{lib.rs (setup call,
  ServerError::Extensions), database.rs (graph mode)}`,
  `forgebase-pgengine/src/{lib.rs, config.rs}`, `forgebase-db/src/{lib.rs,
  sqlite.rs, pg/mod.rs (report slot)}`, the FB-148 test in
  `forgebase-api/src/lib.rs`.

**For the next packages: how a feature asks.**
- **FB-162 (vector):** `db.feature_active(Feature::Vector)` picks the
  pgvector store. The column exists (`vec forgebase.vector`, `vector bytea`
  nullable); add your own DDL as `EXTENSION_DDL` vector version 2+ (never
  edit version 1). Qualify the type and operator classes with
  `report.get(Feature::Vector).schema` (`forgebase` unless an external
  server had it elsewhere). `vector.convert` must update
  `ext.vector.engine` (`extension` | `portable`) in the same transaction as
  its last batch; there is no API for it yet beyond `meta_set`.
- **FB-163 (graph):** `db.feature_active(Feature::Graph)`; false means
  `400 cypher_unsupported`. Managed roles preload AGE once FB-160 ships
  `lib/plugins/age.*`; on external servers the operator must set
  `session_preload_libraries` for the role (the reason says so). Use `SET
  LOCAL search_path = ag_catalog, pg_catalog` per transaction.
- **FB-164 (queue):** `db.feature_active(Feature::Queue)`; pgmq is owned
  by the server role, so `pgmq.create`/`drop_queue` work on the writer, and
  the reader can read queue tables (default privileges). Check the pgmq
  table layout yourself and fall back to portable with a warning (§1.6);
  `queue.convert` updates `ext.queue.engine` like vector.
- **FB-165 (cron):** `db.feature_active(Feature::Cron)` is true only when
  pg_cron is preloaded and `cron.database_name` is this database (the
  bundled engine: `forgebase`); the server role has `USAGE` on `cron`, jobs
  it schedules run as `forgebase_server`. Not stateful: if pg_cron goes
  away, schedules fall back to the Rust tick at the next start (clear stale
  `cron.job` rows when it comes back).
- **FB-166 (trigram):** `db.feature_active(Feature::Trigram)`; false →
  `400 index_kind_unsupported` / `fuzzy_unsupported` on PostgreSQL too.
  pg_trgm is in schema `forgebase`: write `forgebase.gin_trgm_ops` in index
  DDL.
- **FB-153 (SQL editor allow list):** refuse `CREATE/ALTER/DROP EXTENSION`
  for vector, age, pgmq, pg_cron and pg_trgm (§1.1), and `cron.*` (§1.7).

## Addendum: FB-167, replica routing and read-your-writes (2026-11-11)

Built as §2.1 and §2.4–§2.7 describe, inside `forgebase-db` (`routing/`),
`forgebase-api` (`consistency.rs`) and both SDKs. Nothing in the server
opens replicas yet: FB-171 builds the router, FB-168 provisions managed
replicas, FB-169 passes intents at the §2.5 call sites. Without replicas
every read is on the primary and responses are unchanged, on both backends.

**Files.** `crates/forgebase-db/src/routing/` (`routing.rs` moved to
`routing/mod.rs`; new `token.rs`, `scope.rs`, `health.rs`, `check.rs`,
`replicas.rs`, and `testing.rs` behind the `testing` feature); the default
method in `backend.rs`; `SetHandle` replicas, `SetConnector::connect_replica`,
`RouterOptions::replicas` and the reload step in `router.rs`; re-exports in
`lib.rs`; `crates/forgebase-api/src/consistency.rs`; the test files
`crates/forgebase-db/tests/replicas.rs`, `sdk/js/test/consistency.test.ts`;
`sdk/js/src/consistency.ts`, `sdk/rust/src/consistency.rs`. No migration.

**Model.**
- `Backend::fetch_routed(stmt, &ReadIntent)`: a default method that runs
  `fetch_all`. Only `SetHandle` overrides it, and only when the set has a
  `ReplicaSet`. `ReadIntent { what, replica, consistency, read_after }`:
  `ReadIntent::primary(what)` (the default) or `ReadIntent::replica(what)`,
  plus `.with_consistency(Strong)`, `.after(&token)`, `.after_all(&read_after)`.
- `ReplicaSet` (one per set, control included): the replicas as read-only
  `Database`s on each standby, a monitor task, and the choice. Built by
  `DbRouter::reload` from `Topology::nodes_of(set)` with role `replica` and
  state `ready | catching_up | streaming | lagging | unreachable`, opened
  with `SetConnector::connect_replica` (default: refuse, so a connector that
  cannot open replicas leaves every read on the primary) and checked with
  `check_replica`. A node that cannot be opened or fails the check is left
  out with a log line; routing never fails because a replica is missing. A
  reload with the same nodes keeps the set; with other nodes it keeps the
  health, counters and connection of every node it already had.
  `read_routing = "primary"` and SQLite open no replicas.
- `ReplicaOptions` carries `[database.replicas]` (§4.1) with its defaults:
  `read_routing`, `read_your_writes`, `lsn_wait` 50 ms, `lsn_poll` 5 ms,
  `max_lag` 2 s, `max_lag_bytes` 64 MiB, `health_interval` 500 ms,
  `return_after` 3. `health_interval = 0` runs no monitor (tests call
  `refresh`).

**Health (§2.7).** Every sample reads the primary's
`pg_current_wal_insert_lsn()` and, on every replica in parallel,
`pg_is_in_recovery()` and `pg_last_wal_replay_lsn()`. Lag in bytes is the
difference; lag in time is the age of the oldest primary sample the replica
has not replayed (a ring of 256 samples). Deviation: `pg_stat_replication`
is not read; it needs `pg_monitor` for other roles' rows, cannot see
external replicas, and the replica-side position works for every node kind.
States: a new node enters rotation on its first sample within the limits
(`catching_up` → `streaming`); above either limit it is `lagging`; it comes
back after 3 consecutive samples under half of both limits; no answer is
`unreachable` (also set at once when a read finds the replica gone); a node
no longer in recovery is `needs_rebuild`. Only transitions are written, with
`topology::update_node_state` on control (bumps the epoch), in the
background. `ReplicaSet::status()` gives the live view (state, rotation,
replay LSN, lag, in-flight, reads, fallbacks, last error) for FB-175/176.

**Choice and fallback.** Among replicas in rotation (weight > 0, read pool
not saturated by our reads) that have replayed the read's LSN: the lowest
(in-flight + 1) ÷ weight, ties rotating. None has: the most advanced one is
waited on up to `lsn_wait` (`WAIT FOR LSN '<lsn>' WITH (TIMEOUT '<n>ms',
NO_THROW)` on PostgreSQL ≥ 19, outside a transaction on a read connection;
on a parse error it is marked unsupported; otherwise polling every 5 ms),
then the primary. A replica read that fails with `Busy` (SQLSTATE `40001` /
`40P01`: a recovery conflict on a standby; also pool timeouts) or
`Unavailable` / `Closed` / a driver error is retried once on the primary;
statement errors are returned as they are. Metrics:
`forgebase_db_routed_reads_total{set,target}`,
`forgebase_db_replica_fallbacks_total{set,reason}` (`strong`,
`no_replica`, `behind`, `unavailable`, `recovery_conflict`).

**Read-your-writes (§2.6).**
- Commit LSN: the PostgreSQL backend already read
  `pg_current_wal_insert_lsn()` in the `COMMIT` round trip (FB-158 seam);
  kept as is. A `SetHandle` with replicas wraps its write transactions: after
  `COMMIT` it records `<set id>/<Database::last_commit_lsn()>` in the
  request scope. The value is at or after the transaction's own commit (the
  writer is single; a concurrent later commit can only raise it).
- `RequestScope` (a Tokio task-local, `routing::scope`): consistency, the
  positions reads must see, and the commits made. Commits also raise the
  scope's own minimum, so a request reads its own writes. Work spawned onto
  other tasks does not see the scope.
- Tokens: `CommitToken` `<set>/<lsn>` (set ids `[A-Za-z0-9_.-]{1,64}`),
  `ReadAfter` (highest LSN per set; a header lists tokens separated by
  commas, at most 64; malformed ones are ignored, never an error).
- `forgebase-api` `consistency::track`, mounted innermost (inside
  authentication, so the principal is known): reads
  `X-ForgeBase-Consistency` (`strong` | `session`, anything else ignored)
  and every `X-ForgeBase-Read-After` value, adds the principal's recent
  commits (`RecentCommits`: per realm and principal, 10 s, at most 10 000
  principals, least recently written dropped; anonymous and system callers
  are not remembered), runs the request in the scope, and answers
  `X-ForgeBase-Commit` when it committed on a set with replicas. CORS allows
  the two request headers and exposes the response header (shared edit in
  `lib.rs`).
- SDKs: JavaScript `CommitTokens` (`fb.commitTokens`), client option
  `consistency: "session" | "strong"`, per call `{ consistency }` on
  `request`, `collection().list/get`; Rust `CommitTokens`
  (`fb.commit_tokens()`), `ClientBuilder::consistency`,
  `ForgeBase::with_consistency` (shares credentials, tokens and realtime).
  Both read `X-ForgeBase-Commit` from every response (a failed request may
  have committed) and send `X-ForgeBase-Read-After` only once they hold a
  token.

**External checks (§2.4).** `check_replica(primary, replica, name)`: in
recovery, equal `system_identifier` (`pg_control_system()`), version ≥ 16,
and the role has `USAGE` on schema `forgebase` and `SELECT` on
`forgebase._forgebase_migrations`. A mismatch is
`ReplicaCheckError::Mismatch` (`node_mismatch`, 400); a server that cannot be
asked keeps its `DbError` code. `primary_facts(db)` returns the facts
(version, identifier, timeline) for the admin API to store.

**Tests.** `crates/forgebase-db/tests/replicas.rs` builds real standbys
(`routing::testing::ReplicaCluster`: `initdb`, `pg_basebackup -R -X stream`,
each server on its own socket directory, a watchdog per server; gated on
`FORGEBASE_TEST_PG_ENGINE` / `FORGEBASE_PG_ENGINE_DIR`): lag out of rotation
and back after three samples; read-after tokens (wait, then primary; served
by the replica once replayed; strong; `read_your_writes = false`); a
recovery conflict (`max_standby_streaming_delay = 0`, `ALTER TABLE` on the
primary) retried on the primary; another cluster's standby and a primary
refused; the router opening only checked replicas, recording the health
transition and keeping state across a reload. One test runs on the selected
backend without replicas (unchanged behaviour). `forgebase-api`
`consistency` tests: headers, the principal map, and the whole path over
HTTP against a real standby.

**For the next packages.**
- **FB-168 (managed replicas):** insert the node row (`role = replica`,
  state `provisioning`, then `catching_up` once started) and `reload()`; the
  router opens nodes in `ready | catching_up | streaming | lagging |
  unreachable`. `connect_replica` must open the standby with
  `DbOptions::read_only()` (for a local node, its socket directory).
  `NodeState::NeedsRebuild` from the monitor means "not in recovery".
- **FB-169 (call sites, realtime):** pass `ReadIntent::replica("<area>")`
  at the §2.5 call sites through `fetch_routed`; everything else keeps
  `fetch_all`. `AccessContext.read_after` can become `ReadIntent::after_all`;
  the request scope already carries the headers, so a call site inside an
  HTTP request needs nothing more. Realtime pages: sample
  `Database::current_wal_lsn()` and send `<set>/<lsn>`; the SDKs take it
  with `commitTokens.note(...)` / `commit_tokens().note(...)`.
- **FB-171 (server):** pass `RouterOptions { replicas: <from
  [database.replicas]>, .. }`; implement `SetConnector::connect_replica`
  (unseal `url_sealed`). The `[database.replicas]` keys are not in the FB-91
  registry yet (outside this package): add them with `ReplicaOptions`'
  defaults.
- **FB-175 (admin API):** adding an external node runs `check_replica`
  (400 `node_mismatch`) and stores `primary_facts` (version, identifier,
  timeline); show `SetHandle::replicas().status()` for live health.

## Addendum: FB-164, queue deliveries on pgmq (2026-11-11)

Built as §1.6 describes. Where the code differs from the text above, or
decides something it left open, it is said here.

**Files.** `crates/forgebase-queue/src/pgmq/{mod.rs, engine.rs}` (new):
the delivery store over pgmq queues, engine selection, the layout check,
reconcile and `queue.convert`. Engine branches in `consume.rs` (claim,
materialisation, reap, ack, nack, deadline, release, in-flight, seek,
snapshots, dead letters), `ops.rs` (metrics, backlog, retention) and
`service.rs` (queue created/dropped with the subscription and with a
forced topic delete). `model.rs`: `DeliveryEngine`, `ConvertReport`.
Tests: `tests/pgmq.rs` (new), the harness runs FB-161's extension setup on
PostgreSQL, the FIX-9 plan test knows both engines. No migration. Outside
Owns: the restart-reason fix in `forgebase-db/src/pg/extensions.rs` (below).

**Which engine.** The service has no `Database` handle (it gets a
`dyn Backend` through the data core), so it reads what
`Database::feature_active(Feature::Queue)` is derived from:
`_forgebase_meta` `ext.queue.engine = extension` (FB-161 pins it at the
first setup; the setup refuses to start when the pinned extension is
gone). With that pin, pgmq installed and the layout check passing, the
service uses pgmq; otherwise the portable table. It decides once, on the
first delivery operation, and keeps the answer behind a `tokio` `RwLock`:
every delivery operation holds the read guard for its transaction,
`queue.convert` the write guard. `QueueService::delivery_engine()` tells.

**Queue names.** `fbs_` and the first 128 bits of SHA-256 of the stored
subscription name (`<tenant>/<name>` for a tenant's), in hex: 36
characters (pgmq allows 47; stored names are not identifiers). Tenant and
owner stay in ForgeBase's tables (FB-141); the owner's view of record
messages is still applied at claim from `source_row`.

**Entry body.** `{seq, key, lease, consumer, history}`; §1.6 named four
fields, `consumer` is added: it marks a leased entry (`consumer` not
null) and serves streaming flow control and `release_consumer`. `lease`
stays after a lease expires, so a late ack counts until the entry is
leased again (ADR-0022 §4), exactly as `lease_id` does on the portable
table. `read_ct` is the attempt count, `vt` is `available_at` (ready) or
`lease_until` (leased). History entries have the portable shape (`attempt,
at, consumer, outcome, error`, at most 25).

**Clock.** Every `vt` is the service clock's epoch milliseconds as
`timestamptz 'epoch' + ms * interval '1 millisecond'` (exact both ways),
never `now()`: tests drive a manual clock, and both engines must agree.
`pgmq.send_batch(name, msgs, vt timestamptz)` takes it.

**Statements** (all ForgeBase's own over `pgmq.q_fbs_…`):
- materialisation: one `pgmq.send_batch` per claim round of new messages
  (a filter picks in Rust first); `msg_id` order equals `seq` order;
- claim: `SELECT … WHERE vt <= $now [AND no earlier entry with the same
  key] ORDER BY msg_id LIMIT $n FOR UPDATE OF q SKIP LOCKED`, joined with
  the message log, then **one** `UPDATE … FROM jsonb_to_recordset(…)`
  per round sets `vt`, `read_ct + 1`, lease, consumer and the history entry.
  Select-then-update instead of §1.6's single `UPDATE … RETURNING`: the
  byte limit of streaming pull and the owner's view (withheld records are
  deleted, not leased) are decided per message in Rust, as on the
  portable path;
- the reap (expired leases → ready after backoff, or dead letter) runs
  before each claim as on the portable path, over an index ForgeBase adds
  to each queue table: `(vt) WHERE message->>'consumer' IS NOT NULL`.
  Ordering subscriptions also get `((message->>'key'), msg_id) WHERE key
  IS NOT NULL`;
- ack ids are `ack-<msg_id>-<uuid>`: ack is one `DELETE … USING
  jsonb_to_recordset(…) WHERE msg_id = l.id AND message->>'lease' =
  l.lease` for the batch; nack, modify-deadline and release update `vt`,
  the lease and history by `msg_id`. Ack ids from the portable engine
  (before a conversion) are found by their lease;
- dead letters: the entry is read with its message, the dead-letter
  record and the copy on the dead-letter topic are written as before, the
  entry is deleted;
- seek purges the queue and, for a snapshot, inserts its items;
  snapshots read the owed `seq`s from the queue;
- retention: pgmq entries have no foreign key to the message log, so after
  pruning a topic its queues drop entries whose message is gone;
- metrics: per subscription from its queue; `/metrics` backlog adds every
  queue (one `UNION ALL` per 100 queues).

**Layout check.** At the first delivery operation and before converting
to pgmq: pgmq must be installed; a probe queue created in a transaction
that is rolled back, and every existing `pgmq.q_fbs_%` table, must have
`msg_id bigint, read_ct integer, enqueued_at timestamptz, vt timestamptz,
message jsonb` (`format_type`). A mismatch logs a warning and uses the
portable table (the pin stays `extension`; entries in the mismatching
queues stay there, untouched); `queue.convert` to pgmq answers `400
pgmq_unavailable` with the problems.

**Reconcile.** At that first use, and inside every conversion, owed
deliveries are moved to the engine in use, one transaction per
subscription: pgmq creates missing queues and takes any portable rows
(state, attempts, lease, history kept, `seq` order); portable takes back
the entries of every `fbs_` queue (those whose message still exists) and
drops the queues; queues without a subscription are dropped. This also
covers databases set up after FB-161 and before FB-164 (pinned
`extension` with portable rows) — no re-creation needed.

**`queue.convert`.** `QueueService::convert(ctx, to)` (instance-wide
`engine:queue_admin`, no tenant) and the reserved job kind `queue.convert`
(`forgebase_queue::register_jobs`, payload `{"to": "pgmq" | "portable"}`).
It writes `ext.queue.engine` first and then reconciles, under the write
guard: a conversion that stops half way is finished by the next start.
The extension report (`/api/system/info`, `feature_active`) shows the new
engine after the next start.

**Restart reason fix (FB-161 follow-up).** A stateful feature pinned
`portable` in `auto` now reports the extension's own unavailability
reason when it is missing (as the first start did), and the
"`<feature>.convert` moves it" reason only when the extension is usable:
a restart reports the same status as the first start on engines without
pgvector/pgmq.

**Tests.** `tests/pgmq.rs`: one queue per subscription (created, entries
leased/acked, dropped with the subscription and with a forced topic
delete); `queue.convert` both ways with a lease handed out before each
conversion still acked, attempts and history carried (redelivery is
attempt 2), an ordering subscription converted, the stored engine
followed after a restart, `pgmq_unavailable` where pgmq is missing; a
renamed `vt` column falls back to portable and refuses conversion; the
convert job kind is reserved. The rest of the suite (ADR-0022:
fan-out, competing consumers, redelivery, dead letters, ordering, dedup,
replay, restart survival, streaming flow control, push, record views)
runs on whichever engine the database offers: SQLite (portable),
PostgreSQL without pgmq (portable), the bundled engine (pgmq), and the
bundled engine with `FORGEBASE_TEST_PGMQ=off` (portable, and convert
portable → pgmq → portable).

**Bench** (`queue_pull_ack_10`, pull 10 + ack 10 with a standing backlog
of 10 000, through `forgebase_server::start`; bench profile with thin LTO
off for build time; a shared, loaded machine, so the spread is large on
every engine): pgmq on the bundled engine 3.0 / 3.6 / 16.6 / 82 ms per
round (one run at 1.76 s with every round slow, from the warm-up on);
portable on Homebrew PostgreSQL 18 7.1 / 7.3 / 8.9 ms; SQLite 14.5 / 35 /
210 ms. The pgmq round runs fewer statements than the portable one (one
lease `UPDATE` and one ack `DELETE` for the batch instead of one per
message); in-process, without the server, a round took about 2.3 ms in
a debug build after 400 rounds, every statement well under 1 ms.

**For the next packages.**
- **FB-169 (replica reads):** claims, acks, nacks, deadline changes,
  releases, seeks and snapshots write pgmq queue tables: primary only.
  `subscription_metrics` and `backlog` read the queue tables
  (`pgmq.q_fbs_…`, `SELECT` granted to `forgebase_reader` by default
  privileges) and may go through `fetch_routed` with
  `ReadIntent::replica(..)` (FB-167); the queue name of a subscription is
  `crate::pgmq::queue_name(stored name)`.
- **FB-172 (shard routing):** the engine is resolved per `QueueService`,
  i.e. per backend: with one service per set, each set's database keeps
  its own `ext.queue.engine` pin, pgmq install and queues, and resolves
  on its own. A tenant move must move its subscriptions' pgmq queue
  tables (whole tables outside the placement registry; `msg_id` is per
  queue, so `apply_sequence_range` need not touch it) — or convert the
  moving tenant's deliveries to portable rows first and let the target's
  reconcile adopt them (reconcile moves portable rows of any subscription
  into its queue). Maintenance (`run_maintenance`, retention pruning of
  queue entries) is per set.
- **Server wiring (not done here, outside Owns):** register the job kind
  in `forgebase-server/src/lib.rs` next to the other `register_jobs`
  calls: `let registry = match &queue { Some(q) =>
  forgebase_queue::register_jobs(registry, q.clone()), None => registry };`.

## Addendum: FB-165, schedules on pg_cron (2026-11-11)

Built as §1.7 describes. Where the code differs from the text above, or
decides something it left open, it is said here.

**Files.** `crates/forgebase-jobs/src/pgcron.rs` (new: mapping, sync,
catch-up, listener, layout check), call sites in `service.rs`
(`create/update/delete_schedule`, `schedule_from`), `model.rs`
(`Schedule.fired_by`), `lib.rs` (exports). `migrations/{system,postgres}/
0038_schedule_fire.sql` (registered in `forgebase-migrations`'s `lib.rs` and
`postgres.rs`). Outside Owns, needed for the `PgListener` wake-up:
`forgebase-db` `Database::listen(channel) -> Listener` (`src/pg/listen.rs`,
new; a `mod` line and one method). Tests: `crates/forgebase-jobs/tests/
pgcron.rs` (starts a managed engine from `FORGEBASE_PG_ENGINE_DIR`, skips
without pg_cron) and unit tests in `pgcron.rs`.

**Expression mapping (`map_expression`).** Decided by reading pg_cron 1.6's
parser (vixie cron) and a differential test: an independent model of
pg_cron's reading gives the same next 1 000 due times as `Cron::next_after`
for every expression the mapping accepts (19 fixed + 400 generated
expressions); the live half runs the mapped text through the real
`cron.schedule` and checks that the refused ones really are refused. Not
mapped, so they stay on the Rust tick:
- `n/s` (a number with a step): ForgeBase reads "from n to the end", pg_cron
  refuses it; `?`: ForgeBase only;
- a day field that starts with `*` but is not a plain `*` (`*/2`, `5,*`
  with the other day field restricted): pg_cron ANDs day-of-month and
  day-of-week whenever either starts with `*`; ForgeBase ORs unless a field
  has a plain `*` element. The mapping accepts a field only when both
  readings agree on whether it is a star;
- 6-field expressions other than a leading `0` (mapped as the 5-field
  expression) or `*/N * * * * *`. N must divide 60 (1, 2, 3, 4, 5, 6, 10,
  12, 15, 20, 30), because ForgeBase's seconds grid restarts every minute
  (`*/7` is 0, 7, …, 56, 0) and `'N seconds'` does not. These map to
  `'N seconds'`.
Names (`JAN`, `mon-fri`, any case) and `7` = Sunday agree and map.

**`forgebase.fire_schedule(id text, due bigint DEFAULT NULL)`** (PL/pgSQL;
the SQLite file is a comment plus `SELECT 1`). As §1.7, plus: the due time
is the start of the minute, or of the N-second slot for `*/N * * * * *`
(read from the stored expression, so a changed expression needs no new
function), or `due` when the Rust start-up catch-up passes it. The job is
inserted like `insert_job`: key `schedule:<id>:<due ms>` per tenant, the
schedule's priority, `timeout_ms` from `<tenant>/<queue>` queue settings,
`created_by = 'schedule:<name>'`, `waiting_for 0`, tenant and context copied,
the `enqueued` event, `pg_notify('forgebase_jobs', <job id>)`. A test
compares the job it inserts column by column with the one the tick inserts.
`next_run_at` is not touched. pg_cron runs `SELECT
forgebase.fire_schedule('<id>')` (exactly the command FB-146's census
allows). pg_cron seconds jobs are "every N seconds" from when the worker
saw them, not a grid; the function snaps the due time to the grid, so a
slot is fired once (a late firing in a slot already fired is deduplicated).
Realtime job events are not published for jobs inserted by pg_cron (the
sink lives in the server process); the status events of later transitions
are.

**Who fires a schedule: a sentinel, not a column.** A pg_cron-owned
schedule keeps `next_run_at = 253402300799000` (9999-12-31), so the Rust
tick's due query never returns it, with no join to `cron.job` (which the
reader role cannot see) and no new column (the schema-parity test stays
untouched). Reads compute the real next firing from the expression and
return `fired_by = "pg_cron"`; every other schedule says `"forgebase"`.
The sentinel and the `cron.job` row change in one transaction
(`pgcron_reconcile`): creating, updating, enabling a mappable schedule takes
it over; disabling, deleting or an unmappable expression takes the job
away and gives `next_run_at` back (next due time from now). A failed
post-commit sync is logged and repeated every 60 s; until then the
schedule stays with the tick, so it is never fired by neither.

**Engine selection.** Not stateful (FB-161): `attach_pgcron(&JobService,
&Database)` follows `db.feature_active(Feature::Cron)` at each start; there
is no `ext.schedule.engine` pin and no convert job (the schedules are the
state, `cron.job` is derived). Layout check before attaching:
`forgebase.fire_schedule(text, bigint)` exists and `cron.job` has text
columns `jobname`, `schedule`, `command`; a mismatch logs a warning and
leaves every schedule on the tick. When the feature is not active but
pg_cron is installed (mode `off`, preload removed), the same call removes
ForgeBase's `fbsched_*` jobs and hands every schedule back.

**Catch-up.** At attach, a schedule with a due time since max(`last_run_at`,
`updated_at`, `created_at`) is fired once with the first missed due time (a
schedule handed over from the tick uses its recorded `next_run_at` if it
passed): missed firings collapse into one (ADR-0007 §6). A second start with
nothing missed fires nothing.

**Wake-up.** `Database::listen("forgebase_jobs")` opens a dedicated
connection (sqlx `PgListener`, reader pool's options); a task wakes the
worker pool on every notification and once after each reconnect (messages
sent while it was away are lost, so a wake means "look").

**Tests.** `tests/pgcron.rs`: `fire_schedule` equals the tick (PostgreSQL),
keys per slot, idempotence, disabled and unknown ids, NOTIFY payload; the
fallback without an active pg_cron (every backend); on the bundled engine:
pg_cron fires a `*/2` seconds schedule, the pool is woken by NOTIFY, the
`cron.job` row is exact, mappable ↔ unmappable ↔ disabled ↔ enabled ↔
deleted moves the schedule between pg_cron and the tick, the census (only
`fire_schedule` commands); restart catch-up fires once, a second attach
adds nothing, a non-active attach hands everything back; the real pg_cron
accepts every mapped text and refuses the unmapped ones.

**Server wiring (not done here, outside Owns).** In
`forgebase-server/src/lib.rs`, after `start_pool` and with the other
schedule owners registered: `let pgcron = forgebase_jobs::attach_pgcron(&jobs,
&db).await;` and keep `pgcron` (a `PgCronHandle`) in the server handle next
to `job_pool`; dropping it stops the listener and the 60 s check.

**For FB-169 (replicas).** pg_cron, `fire_schedule` and the sync all write:
run `attach_pgcron` on the primary's handle only (pg_cron idles in a hot
standby and starts on promotion; after a promotion call `attach_pgcron`
again, it is idempotent). The `LISTEN` connection uses the reader pool's
primary options; it must stay on the primary (NOTIFY is not replicated to
standbys' listeners).

**For FB-172 (tenant moves).** Schedules carry `tenant`; a moved tenant's
schedules must move with its jobs, and each set's server attaches to its
own database: after the move, `pgcron_reconcile` on the target creates the
`fbsched_*` jobs (the reconcile runs at attach) and on the source removes
them (the schedule row is gone). The sentinel travels with the row, so a
move must be followed by an attach/reconcile on the target before the
tick is relied on. `idempotency` keys are per tenant, so a due time already
fired on the source is not re-fired as the same key on the target only if
the job rows moved too.

## Addendum: FB-162, vector search on pgvector (2026-11-11)

Built as §1.4 describes. Where the code differs from the text above, or
decides something it left open, it is said here.

**Files.** `crates/forgebase-vector/src/{pg.rs, jobs.rs}` (new): pgvector
SQL (layout check, index specs and DDL, searches, recall, build progress,
conversion batches) and the `vector.convert` job kind. Engine branches in
`service.rs`; engine-aware vector reads and writes in `store.rs`; the
pgvector state of a field in `index.rs` (`pg_valid`, `pg_bytes`,
`pg_indexed`); `VectorEngine` and `ConvertReport` in `model.rs`. Tests:
`tests/pgvector.rs` (new); the harness runs FB-161's extension setup on
PostgreSQL (`FORGEBASE_TEST_PGVECTOR=off` keeps the vectors portable); the
four tests about index files know both engines. No migration, no new
`EXTENSION_DDL` step (version 1's column is all the engine needs; the
indexes are per field, created at run time). Outside Owns, in
`forgebase-db`: `Backend::maintenance` and `Decision::Prepare` (below).

**Which engine.** `VectorService` has only a `dyn Backend`, so like FB-164
it reads what `feature_active(Feature::Vector)` is derived from: PostgreSQL
with `_forgebase_meta` `ext.vector.engine = extension` and a pgvector that
passes the **layout check** — the extension installed, version 0.7 or
later (`halfvec`), `_forgebase_vector_values.vec` of type `vector`, the
portable `vector` column nullable. A pinned database that fails it logs a
warning and runs portable (`vector.convert` to pgvector then answers `400
pgvector_unavailable` with the problems). Iterative scans are used from
pgvector 0.8. The engine is resolved in `VectorService::open` and kept
behind a `tokio` `RwLock`: every operation holds the read guard for its
whole run, a conversion the write guard. `VectorService::storage_engine()`
tells.

**Reconcile at open.** Before serving, vectors in the other column move to
the engine in use, in batches: under an `extension` pin, `vector` bytea
rows become `vec` (databases set up between FB-161 and FB-162 — no
re-creation needed — and a conversion back to portable that stopped before
its last batch); without it, `vec` rows go back to bytea (a conversion to
pgvector that stopped early). On pgvector, missing or invalid HNSW indexes
are then built in the background (reason `missing`; searches are
`exact_fallback` meanwhile, `vector_index_not_ready` above
`max_exact_scan`), and `fbv_` indexes no field owns are dropped; on the
portable engine leftover `fbv_` indexes are dropped. Each check is one
scan of the values table per start.

**Storage.** `vec` holds the vector, `vector` stays `NULL`. Values travel
as pgvector's text form, bound as text and cast (`CAST(CAST($n AS text) AS
forgebase.vector)`); Rust prints the shortest decimal that reads back as
the same `f32` and pgvector prints the same, so the stored vector is the
returned one (unit test and `tests/pgvector.rs`). No statement ever
returns a `vector` column (sqlx would look its type up, FB-161's bug).
Types, operator classes and operators are qualified with the extension's
schema (`OPERATOR(forgebase.<=>)`).

**Index.** `fbv_<collection id>_<12 hex>`, the hash over the field's
name, creation time, dimension, metric, element type and build parameters
(a re-created field never meets a leftover index); 40 characters at most.
`CREATE INDEX CONCURRENTLY IF NOT EXISTS … USING hnsw ((CAST(vec AS
forgebase.T(dim))) forgebase.T_<cosine|l2|ip>_ops) WITH (m, ef_construction)
WHERE collection_id = <id> AND field = '<field>'`, T as §1.4 (`vector` for
`f32` up to 2 000, `halfvec` for wider `f32` up to 4 000 and for every
`f16`/`i8` field up to 4 000; wider: no index, exact search only, reason
in the stats). `m` = connectivity clamped to 2…100 and `ef_construction`
= expansion_add clamped to 2·m…1 000 (pgvector's limits; the stats report
the `m` used). Searches repeat the cast and the predicate **as literals**
(collection id and field name are inlined, everything else is bound), so
even a generic plan matches the partial index. The index of a new field is
built when the field is created (`create_collection`, `add_field`) before
the call returns: the field is empty, but `CREATE INDEX CONCURRENTLY`
still scans the table twice, concurrently with writes. Dropping a field or
a collection drops its indexes concurrently after the commit (the next
start drops what a failure leaves).

**Maintenance connection (`forgebase-db`, outside Owns).** New default
method `Backend::maintenance(&[String])`: the statements run in order, each
in autocommit, on a connection of their own opened with the writer's
options (not the writer, not a reader), closed afterwards; `Database` and
`SetHandle` (its primary) delegate, SQLite refuses (`Unsupported`). Where
a backend refuses, the vector engine falls back to the plain statements in
a schema-change transaction on the writer, with a warning.

**Distances** as §1.4; every returned distance is computed on the stored
`float4` vector (`vec <op> query`) even when a `halfvec` index orders the
scan, and L2 is squared internally like the portable path, so both engines
return the same numbers.

**Strategies** keep ADR-0012's names and rules:
- `exact`: explicit, or at most `exact_threshold` vectors (with a filter:
  matches; counted with `LIMIT threshold + 1`), or a field with no index;
  `ORDER BY <distance on the uncast column>, key`.
- `ann`: `hnsw.ef_search = max(expansion_search, k)` (≤ 1 000), no
  iterative scan (pgvector then returns up to `ef_search ≥ k` rows).
- `filtered_ann` (pre/auto) and `post_filter`: `hnsw.iterative_scan =
  relaxed_order`, `hnsw.max_scan_tuples` 20 000, the compiled filter
  (caller filter AND FB-139's collection rule) in the same `WHERE` through
  the items join; results re-sorted. Fewer than `k`: pre/auto search the
  matching set exactly when it has at most `max_exact_scan` vectors (as the
  portable path does); then both widen the scan (ADR-0012's over-fetch
  loop: `max_scan_tuples` ×4 up to the field's size; `ef_search` up to
  1 000 before pgvector 0.8) until `k` match or the field is exhausted.
  `candidates` is reported only by the strategies that counted it (the
  exact ones); `filtered_ann` reports none (the portable path reports the
  number of matches it collected).
- **Settings per search.** Each read is one statement in its own `READ
  ONLY` transaction (FB-131), so `SET LOCAL` has no place. The statement
  is `SELECT … FROM (SELECT set_config('hnsw.ef_search', $ef, true) AS ef,
  …) cfg CROSS JOIN LATERAL (… WHERE … AND cfg.ef IS NOT NULL … ORDER BY …
  LIMIT k) s`: the lateral reference makes the settings run before the
  index scan starts, and the read's `ROLLBACK` ends them. Tested by the
  row count of a non-iterative scan (`ef_search` 5 → 5 rows).
- `VectorService::explain_search` (administrators) returns PostgreSQL's
  plan of a search's index statement; the plan test checks `Index Scan
  using fbv_…` with and without a filter.

**Rebuild** = `REINDEX INDEX CONCURRENTLY` (an invalid or missing index:
`DROP INDEX CONCURRENTLY IF EXISTS` + `CREATE INDEX CONCURRENTLY`) on the
maintenance connection, in the background; the old index serves meanwhile
(`state building`, `serving ann`). The connection's `application_name` is
`forgebase vector build <index>`; every 200 ms the build's
`pg_stat_progress_create_index.tuples_done` (pgvector reports the tuples
it added) becomes `progress.done`, `total` = the field's vectors. The poll
runs on the writer: a separate reader role (`forgebase_reader`) does not
see the server role's progress columns. A rebuild holds the engine's read
guard, so a conversion waits for it.

**Stats.** `engine "pgvector"`, `engine_version` = `extversion`,
`indexed_vectors` = the field's vectors while the index is valid (0
otherwise), `memory_bytes` = `serialized_bytes` = `pg_relation_size` of
the index, `capacity 0`, `dirty false`, no `last_saved`. Tenant and server
totals use the values last seen by a stats call or a build.

**Recall in SQL.** The samples are the field's keys at `s·n/samples`, as
on the portable path. One statement runs every sample's index scan
(`ann_ms`); a second runs every sample's exact top `k` on the uncast
column and compares in SQL: per sample, how many ANN results are at least
as close as the `k`-th exact one (`+ 1e-5 · max(|kth|, 1)`, ties count),
averaged (`exact_ms`).

**Hybrid search.** Unchanged from FB-147: the keyword side reads the
`tsvector` of `_forgebase_vector_fts` (`websearch_to_tsquery` with quoted
words, not §1.4's `plainto_tsquery`: it keeps the `or` operator of
`text_operator`), the same filter ANDed in, RRF unchanged.

**`vector.convert`.** `VectorService::convert(ctx, to)` (instance-wide
`engine:vector_admin`, no tenant; `400 invalid_request` with one) and the
reserved job kind `vector.convert` (`forgebase_vector::register_jobs`,
payload `{"to": "pgvector" | "portable"}`; `Invalid` errors are fatal,
others retried). Under the write guard:
- to pgvector: bytea → `vec` in batches (one transaction each, `SELECT …
  FOR UPDATE` then one `UPDATE … FROM jsonb_to_recordset`; at most
  2 000 000 floats a batch), then every field's index is built
  concurrently and the collection's index files are deleted;
- to portable: `vec` → bytea, the `fbv_` indexes dropped concurrently, and
  USearch rebuilds every field in the background (reason `missing`).
- **Pin.** As FB-161 asked, the last batch's transaction (the one that
  moves fewer than a batch, possibly none) also writes `ext.vector.engine`.
  A conversion that stops earlier leaves the database on its old engine;
  the next start's reconcile moves the converted rows back. (FB-164 writes
  its pin first and finishes forward; both are consistent, this one keeps
  the database on the engine its pin names.) The report (`from`, `to`,
  `vectors`, `indexes`) is the job result; `/api/system/info` shows the
  new engine after the next start.
- **Extension preparation (`forgebase-db`, outside Owns).** A database
  pinned `portable` in mode `auto` never got pgvector created (FB-161 only
  created extensions in use), so nothing could convert it. New
  `Decision::Prepare`: a stateful feature pinned portable in `auto` whose
  extension is usable gets the extension created and its DDL applied, and
  stays portable (state `portable`, reason "… `<feature>.convert` moves
  it"). Applies to queue (pgmq) too: `queue.convert` to pgmq now works on
  such a database. FB-161's test updated (`vector` is installed there now,
  `schema_version 1`, pin still `portable`).

**Tests.** `tests/pgvector.rs`: one partial index per field with the cast,
opclass, `m`/`ef_construction` and predicate for cosine/`f32`,
L2/`f16`, dot/`i8`, 3 000-dim `f32` (`halfvec`) and 4 096-dim (none,
exact, reason in the stats), stored vectors returned exactly, float4
distances through a `halfvec` index; the plan scans `fbv_` with and
without a filter; the settings apply before the scan; a selective filter
returns `k` matches in pre and post mode; a rebuild swaps in a new index
(`REINDEX CONCURRENTLY`) while writes go on; `vector.convert` both ways
with two fields (vectors, versions, metadata, text and exact answers
identical, pin, column counts, index count, stats engine, writes after the
conversion, restart follows the pin, converting to the current engine
moves nothing; tenants and non-admins refused; `pgvector_unavailable`
without pgvector and on SQLite); reconcile at open both ways; a renamed
`vec` column falls back to portable and refuses conversion; the job kind
is reserved; an `#[ignore]`d bench. The rest of the suite (SPEC #12, #13,
#23, filters, tenant isolation, hybrid, validation, recall, HTTP) runs on
whichever engine the database offers.

**Bench** (`bench_recall_and_latency`, 10 000 × 128 cosine, 200 recall
samples, 200 searches per strategy, debug build of the Rust side, a shared
and loaded machine):

| PostgreSQL 18 | load + index | recall@10 | ANN p50 / p95 / p99 | filtered (⅓ of rows) p50 / p95 / p99 | exact p50 / p95 / p99 |
|---|---|---|---|---|---|
| bundled engine, `pgvector` (HNSW in the database) | 38.9 s | 0.975 | 2.24 / 3.72 / 4.95 ms | 2.07 / 2.72 / 3.17 ms | 48.1 / 136.2 / 224.0 ms |
| Homebrew, no pgvector (`portable`, USearch in process) | 4.0 s | 0.952 | 3.20 / 10.04 / 19.43 ms | 16.06 / 47.83 / 98.20 ms | 100.9 / 262.5 / 339.8 ms |

Load + index is the upserts (in batches) until the field is ready. On
pgvector the index exists from `create_collection` on, so each insert
writes through HNSW; the portable path stores bytea rows and inserts into
the in-process graph, which is cheaper. Building after the rows is faster
in pgvector, but no API defers the index during a bulk import yet (open
follow-up). The recall check itself
took 227 ms (ANN side) and 16.0 s (exact side, 200 sequential scans in one
statement) on pgvector, 55 ms and 1.5 s on portable. Filtered search on
pgvector is the iterative scan (`relaxed_order`) of the same index; on
portable it is the in-process filtered ANN.

**For the next packages.**
- **FB-169 (replica reads):** search, hybrid, get, list and stats only
  read (`fetch_all`; the search statement's `set_config` is local to its
  read-only transaction), so they may use `fetch_routed` with
  `ReadIntent::replica(..)` — HNSW indexes are WAL-logged. Primary only:
  upserts, deletes, collection and field DDL, index builds and drops
  (`Backend::maintenance` refuses on a read-only handle), the build
  progress poll (it reads `pg_stat_progress_create_index` of the primary's
  session), `vector.convert`, the reconcile at open, and the recall check
  only if its numbers must match the primary's index exactly. A replica's
  index is valid when the primary's is.
- **FB-172 (shard routing):** the engine is resolved per `VectorService`,
  i.e. per backend: one service per set reads that set's
  `ext.vector.engine` pin, pgvector and layout, and converts on its own.
  Rows keep their ids in a move (§3.3), so a collection's index names are
  the same on the target, but the indexes are not moved: build them there
  after the copy (the target service's `repair_indexes` at open does it, as
  does `rebuild`), and drop the source's with its rows (`delete_collection`
  does; rows deleted otherwise leave orphans, dropped at the source's next
  open). The copy must carry `vec` or the bytea, whichever the source uses;
  when the two sets use different engines, the target's reconcile at open
  moves the rows into its own column. A service learns of collections only
  at open, so the target needs a reopen (or a reload hook) after a move.
  Index builds and conversions are per set.
- **Server wiring (not done here, outside Owns):** in
  `forgebase-server/src/lib.rs` (`start_on`), right after the
  `let vector = if config.vector.enabled { … };` block and before
  `start_pool(…, registry, …)`, as FB-164 did for `queue.convert`:
  `let registry = match &vector { Some(v) =>
  forgebase_vector::register_jobs(registry, v.clone()), None => registry };`.

## Addendum: FB-166, trigram indexes, fuzzy search, full-text query (2026-11-11)

Built as §1.8 describes. Where the code decides something the text left
open, it is said here.

**Files.** `crates/forgebase-data/src/{indexes.rs, list.rs}`; one-line
shared edits in `forgebase-data/src/collections.rs` (the table-rebuild match
treats `Trigram` like `Btree`), `forgebase-db/src/{backend.rs, lib.rs,
router.rs}` (see Backend below), `forgebase-api/src/{http.rs,
v1/records.rs, v1/schemas.rs}` (the query-string parameters and their
OpenAPI text). Tests: `crates/forgebase-data/tests/trigram.rs`. No
migration, no change in `forgebase-query` (the `~` compile already renders
`ILIKE`, which the trigram index serves).

**Full text.** `search` on PostgreSQL is `plainto_tsquery('simple',
_forgebase_unaccent($n))` (was `websearch_to_tsquery`): words are ANDed and
`-`, `OR`, quotes are text, as on SQLite (FTS5 words are quoted). The
tsvector expression is unchanged, so existing GIN indexes still match.
`forgebase-db`'s `PgDialect::fulltext_match` and the vector hybrid keyword
query still use `websearch_to_tsquery` (not on the record-list path; FB-162
owns the latter).

**Backend.** `Backend::feature_active(Feature) -> bool` (default `false`;
`Database` and `SetHandle` delegate) lets the data service ask whether
pg_trgm is in use without holding a `Database`. FB-167's `SetHandle` impl got
the delegating method.

**Trigram index.** `IndexKind::Trigram` (`"trigram"`): one GIN index, one
`forgebase.gin_trgm_ops` key per field (`CREATE INDEX n ON t USING gin
((col) forgebase.gin_trgm_ops, …)`), not unique, not partial (`400
invalid_index`). No cast, so `~` (`ILIKE`) and `%` on the plain column match
it (plan test). Creation answers `400 index_kind_unsupported` when
`db.feature_active(Feature::Trigram)` is false: SQLite, `trigram = "off"`, or
no extension. **Stored as a `btree` row:** `_forgebase_indexes.kind` has a
CHECK for `btree|fulltext` and FB-166 adds no migration, so the row says
`btree` and `definition` says `"kind":"trigram"`; code reading the kind
column alone sees `btree`. If a migration is ever added, widen the CHECK on
both engines and store `trigram`. `IndexInfo.kind` reads the definition.

**`fuzzy`.** List parameters `fuzzy`, `fuzzy_fields` (comma list), `fuzzy_threshold`
(0–1, else `400 invalid_fuzzy_threshold`); also in the POST query body.
Blank `fuzzy` is ignored. Where pg_trgm is not in use: `400
fuzzy_unsupported`. Each field must be a key of a trigram index and readable
by the caller, else `400 no_trigram_index` (masked fields refused exactly as
for `search`; with no `fuzzy_fields`, every key of the collection's trigram
indexes is used, so one masked key refuses the whole query for that caller).
SQL: `col % $n` OR-ed over the fields; ordered by `GREATEST(similarity(col,
$n)…)::float8 DESC, id` unless `sort` is given (then `sort` wins and the
match is a filter). Cursor pagination works on the ranking (the similarity
is the first key of the cursor).

**Threshold semantics.** `%` uses the server's `pg_trgm.similarity_threshold`
(0.3 by default), and a request cannot change a GUC on a pooled read. So:
no `fuzzy_threshold` → `%` only (index assisted); ≥ 0.3 → `% AND
similarity >= t` (index assisted); < 0.3 → `similarity >= t` only (correct,
not index assisted: a sequential scan). A server whose GUC differs from 0.3
shifts the 0.3 boundary.

**Extension off.** Tested: PostgreSQL with `trigram = "off"` answers like
SQLite (`index_kind_unsupported`, `fuzzy_unsupported`), `~` unindexed.
An existing trigram index survives the extension going away only as far as
PostgreSQL allows (dropping pg_trgm cascades or refuses; FB-161 never drops
extensions).

**New API parameters (OpenAPI fixtures and SDKs).** Query string and POST
`.../records/query` body: `fuzzy` (string), `fuzzy_fields` (array / comma
list), `fuzzy_threshold` (number). Index kind value `trigram` in the index
definition. New error codes: `index_kind_unsupported`, `fuzzy_unsupported`,
`no_trigram_index`, `invalid_fuzzy_threshold` (all 400).

**Breaking.** `search` no longer interprets `-word`, `OR` or quotes as
operators on PostgreSQL (it never should have; SQLite never did). `ListQuery`
has three new fields (all `Default`); `Backend` has a new defaulted method.

**Notes for next packages.**
- FB-169 (replica reads): `list_records` and `explain_list` read the trigram
  catalogue (`_forgebase_indexes`) through `backend.fetch_all`; the list
  itself already goes through `read_rows`. A replica needs pg_trgm and the
  same indexes (streaming replication carries both); `feature_active` is the
  primary's view.
- FB-172: `trigram_indexes` is one query per fuzzy request; cache it with the
  catalog snapshot if it shows in profiles.
- Dashboard (database studio): index kind selector gains `trigram`
  (PostgreSQL with `database.features.trigram.state = active` only); list
  screens may offer a fuzzy toggle with a threshold slider. `explain` shows
  the trigram index in `indexes_used`.

## Addendum: FB-173, schema fan-out, migrations and native RLS across shards (2026-11-11)

Built as §3.7 and §3.10 describe, on FB-170's router and FB-152's policy
compiler. Without shards nothing changes: every path below checks
`DbRouter::is_sharded()` (more than one routable set) and otherwise commits
its one transaction as before, so SQLite and single-server PostgreSQL
behave exactly as today. Where the code differs from the text above, or
decides something it left open, it is said here.

**Files.** New: `crates/forgebase-db/src/{twophase.rs, shards.rs,
shard_testing.rs, pg/twophase.rs}`, `crates/forgebase-migrations/src/multi.rs`,
`crates/forgebase-policy/src/native/fanout.rs`, the migration pair
`migrations/{system,postgres}/0041_index_builds.sql`, and the tests
`crates/forgebase-db/tests/{twophase.rs, shard_rollout.rs}`,
`crates/forgebase-data/tests/shards.rs`,
`crates/forgebase-policy/tests/shards.rs`. Changed: the schema path of
`crates/forgebase-data/src/collections.rs`; small shared edits listed at the
end.

**Two-phase commit (`forgebase_db::twophase`).**
- `WriteTx::prepare(gid)` ends a transaction with `PREPARE TRANSACTION`
  (default: roll back and refuse, so SQLite never prepares). The engine
  side (`pg/twophase.rs`) checks the identifier (`[a-z0-9_]`, ≤ 199 bytes),
  runs `COMMIT PREPARED` / `ROLLBACK PREPARED` on the writer outside a
  transaction block (`Ok(false)` for an unknown identifier, `42704`), and
  bumps the schema generation after a commit (prepared DDL becomes visible
  then). `55000 prepared transactions are disabled` becomes a message that
  names `max_prepared_transactions`.
- Identifiers: transaction `fb_<32 hex>`, part `fb_<32 hex>_<set number>`
  (≤ 41 bytes; the journal's 180-character limit is far away).
- `TwoPhase::new(control, purpose)` → `prepare(participant, tx)` per node →
  `commit()` (journal row `committing` in its own control transaction:
  that row is the decision; then `COMMIT PREPARED` on every part; then
  `committed` with `resolved_at`) or `abort()`. A part whose `COMMIT
  PREPARED` fails is reported in `Committed.pending` and left to recovery;
  the change counts as committed. If the journal insert fails, the row is
  read back: absent → roll back everything; present → go on; unreadable →
  `TwoPhaseError::Unknown` (`schema_change_in_doubt`, 503) and recovery
  decides. Dropped undecided (a cancelled request), the coordinator rolls
  its prepared parts back in the background.
- `run(control, participants, purpose, mode, &dyn NodeWork)` is the
  one-call form; one participant is one ordinary transaction (`Ok(None)`).
  Parts run one after the other (fewer simultaneous locks; schema changes
  are rare).
- `check_supported` refuses a node with `max_prepared_transactions = 0`
  (`TwoPhaseError::Unsupported`, `sharding_unsupported`, 400); the
  coordinators check every node before preparing anything.
- **Recovery** (`recover(control, nodes, grace)`): reads each node's
  `pg_prepared_xacts` (`fb_` prefix, this database); a part whose
  transaction has a journal row is committed, one without a row is rolled
  back once older than `grace` (30 s, `RECOVERY_GRACE`) **and** not in
  flight in this process (an in-process registry of live coordinators,
  so the minute loop never rolls back a coordinator between its prepares
  and its journal row). A `committing` row is closed (`committed`) only
  when every set it names was read and holds no part; committed rows are
  pruned after 7 days. Unreachable sets are reported, never fatal.
  `spawn_recovery(router, 60 s)` runs it at start and every minute
  (`RecoveryTask`, aborted on drop).
- `in_doubt(control, nodes, grace)` (doctor): prepared parts older than the
  grace (with the journal state: recovery commits / rolls back), decided
  rows still `committing` after 5 minutes (`Stuck`), unreadable sets;
  `describe_in_doubt` renders one line.
- Deviation: control's own part is prepared like the others (control is
  shard 0 and holds catalog rows and tenant tables), and the journal row is
  written in a separate control transaction after every part, control's
  included, is prepared.

**Collection DDL on every shard (`forgebase-data`).**
- `DataService::attach_shards(router)` (once). Create, update and delete
  collection keep doing everything on control in their transaction
  (catalog rows, history, index rows, native policies), then
  `finish_schema` prepares that transaction and repeats the **DDL step**
  (`ShardStep`) on each shard in its own `SchemaChange` transaction, then
  commits all as one two-phase transaction. Any failure (a stray table on
  one shard, a deferred constraint) rolls back everywhere.
- The step re-runs the DDL generator on each node rather than replaying
  control's statement log: constraint names and dependent views are read
  from that node's catalog (after a rename, PostgreSQL's generated
  constraint names differ between a table created long ago and one
  replayed under its new name). Control's catalog snapshot and its
  managed-index rows are passed in (`alter_table(…, managed,
  catalog_rows = false)`): shards have the `_forgebase_indexes` table but
  no rows (class `control`). Each shard part also runs
  `NativePolicies::before_change` / `after_change` and
  `verify_collections` against control's catalog.
- **Catalog replay** (`DataService::replay_catalog(target)`, also
  `forgebase_db::shards::CatalogReplay`): one transaction on the new shard;
  tables in creation order without relation columns to other collections,
  then those columns (`ALTER TABLE … ADD COLUMN … REFERENCES`), so relation
  cycles replay; tenant, automatic and managed indexes; views from
  `pg_get_viewdef` on control (in creation order); `verify_collections`;
  native policies per collection.
- **Per-set index builds** (§3.7). The index manager's `create_index`
  builds control's copy as before, then each shard with `CREATE INDEX
  CONCURRENTLY IF NOT EXISTS` on a connection of its own
  (`Database::run_autocommit`), recording `_forgebase_index_builds
  (index_name, set_id, state building|ready|failed, error)` on control
  (migration 0041; placement class `control`; RLS enabled as FB-152 asks of
  post-0040 tables). A failed build drops the invalid index on that shard
  and records the error; the call itself succeeds (control has the index).
  No row = built with its collection (schema transaction or replay).
  `drop_index` drops on every shard (`DROP INDEX CONCURRENTLY IF EXISTS`);
  rows go with the catalog row (FK cascade). `DataService::index_builds`
  reads them. Indexes that a schema change adds (automatic indexes) are
  built inside the two-phase transaction, not concurrently.

**Native policies on every shard (`forgebase-policy`, `native/fanout.rs`).**
- Rule changes (`put_policy`, `delete_policy`): `regenerate_in` now returns
  the generated statements; `commit_native` prepares control's transaction
  and applies the **same** statements on every shard (generated once;
  relation helpers are named by a hash, so equal everywhere) in one
  two-phase transaction (journal purpose `native policies`).
- Schema changes: the data-core hook runs inside each shard's part (above).
- Startup (`sync_native_rls`): control, then each shard on its own, each
  with its own fingerprint in its per-set `_forgebase_meta` (idempotent, so
  no two-phase commit is needed).
- Roles: `forgebase_rls` / `forgebase_login` come from migration 0040 on
  every node. Direct logins (FB-153): `create_login_role(db, LoginRole {
  name, verifier })` on the tenant's shard primary (`LOGIN`, no other
  attribute, member of `forgebase_login`; name and SCRAM verifier checked
  and inlined, the password never reaches ForgeBase), `drop_login_role`,
  `login_role_verifier` (`pg_authid`, superuser), `copy_login_role(from,
  to, name, verifier)` for a move (same verifier).

**Migrations over every set (`forgebase_migrations::multi`,
`forgebase_db::shards::migrate_sets`).**
- `migrate_all(control, shards, MultiOptions { max_parallel: 4 }, hooks)`:
  control first; if it fails, no shard is touched (`Skipped`); then shards
  through `buffer_unordered(4)`, each with its own `_forgebase_migrations`
  and pre-migration snapshot (`<backups>/sets/<set id>`, FB-150's
  provider); a failing shard does not stop the others. Shards already at
  head are not announced. The node trait returns `NodeError { code,
  message }` (a node can fail for reasons other than a migration).
- `migrate_sets(router, backups, options)`: marks a shard with pending
  migrations `migrating` (`topology::update_set` + `reload`) before, and
  back to its previous state after; a failure leaves it `migrating`. A set
  left `migrating` by a crashed run that is now at head is made `active`.
- Routing: `for_tenant` / `try_for_tenant` answer
  `RouterError::SetMigrating` (`shard_migrating`, 503) for tenants placed
  on a `migrating` set; control and its tenants are unaffected.

**New shard (`shards::activate_shard(router, set, db, ShardSetup)`).**
Refuses a non-shard, SQLite, and nodes (control or the shard) with
`max_prepared_transactions = 0` (`RouterError::Unsupported`,
`sharding_unsupported`, 400) before changing anything; migrates the
shard; confines control's sequences to set 0 when it is the first shard;
applies the shard's range; replays the catalog; publishes and subscribes
(`control_conninfo`); marks it `active`; reloads. Safe to run again. The
caller (FB-174's `database.shard.add`) provisions the node and inserts
the set and node rows first.

**`control_replicated` (§3.2).** `ensure_control_publication(control)`:
`CREATE` / `ALTER PUBLICATION fb_control FOR TABLE` the registry's
`control_replicated` tables that exist (today
`_forgebase_resource_permissions`; FB-153 adds `_forgebase_db_logins` to
the registry and it is picked up). `subscribe_to_control(shard, control,
k, conninfo)`: the slot `fb_control_<k>` is created on control first
(`pg_create_logical_replication_slot`, which also works when both
databases are in one cluster), then `CREATE SUBSCRIPTION fb_control_<k> …
WITH (create_slot = false, copy_data = true, origin = none)`.
`unsubscribe_from_control` detaches and drops both;
`control_subscription` reports enabled and tables ready. Foreign keys on
the shard's copies are not checked by the apply worker (replica role).

**Doctor.** `forgebase doctor` gains `two_phase`: control's prepared parts
and stuck journal rows (warn, with what recovery will do), and a fail when
the topology has shards but control has `max_prepared_transactions = 0`.
It reads control only: shards are reached by the server's connector.

**Tests** (real, separate clusters from `shard_testing::TestCluster`:
`initdb` in a temporary directory, Unix socket only,
`max_prepared_transactions = 16`, `wal_level = logical`, stopped in `Drop`
and by a watchdog; `Sharded` adds a router, a map connector and
`provision`): commit on three nodes; a failure on one rolls back all with
no journal row; abort and drop; crash of a node after its prepare without
a decision (rolled back after the grace, kept before it); crash after the
decision (recovery commits on restart and closes the row; the row stays
open while the node is down); in-flight coordinators skipped; stuck rows
reported; `max_prepared_transactions = 0` refused; activation (migrated,
sequence ranges on both sides, ACL rows replicated, idempotent,
unsubscribe); `migrate_sets` re-applying a migration with
`shard_migrating` meanwhile; refusal before any change; collection create /
update (rename with a view) / delete on three nodes; a stray table on one
shard rolling back a create everywhere; replay with a relation cycle,
tenant scope, a view and a manual index; per-set index builds with one
failing on duplicates; policies equal on control and shard after replay,
rule changes, schema changes and startup sync; login roles and verifiers.
Migration-runner ordering and the four-at-a-time cap are unit tests with
fake nodes.

**Shared edits outside Owns.** `forgebase-db`: `backend.rs`
(`WriteTx::prepare`), `pg/mod.rs` (module line, `prepare` in `PgTx`'s
impl), `routing/replicas.rs` (`TokenTx` forwards `prepare`), `router.rs`
(two `RouterError` variants, the `migrating` check in `State::route`),
`placement.rs` (`_forgebase_index_builds`), `lib.rs` (modules),
`Cargo.toml` (`time`). `forgebase-migrations`: `lib.rs` / `postgres.rs`
(0041, `pub mod multi`), `Cargo.toml` (`futures-util`). `forgebase-data`:
`lib.rs` (the `shards` field, `IndexBuild` re-export), `indexes.rs` (one
call after `create_index` and `drop_index`). `forgebase-policy`:
`native/mod.rs` (module, `sync_native_rls` per set, `regenerate_in`
returns statements), `store.rs` (`write_native` commits through
`commit_native`). `forgebase-cli`: `doctor.rs` (`check_two_phase`).
`Database::run_autocommit` overlaps FB-162's `Backend::maintenance`
(same idea, its own connection); once both are on main one can call the
other.

**Server wiring (not done here; FB-171 / god).** After `DbRouter::open`:
`data.attach_shards(router.clone())` before `enable_native_rls` (so the
startup sync covers shards); `let recovery =
forgebase_db::twophase::spawn_recovery(router.clone(),
twophase::RECOVERY_INTERVAL)` kept in the server handle;
`forgebase_db::shards::migrate_sets(&router, Some(&backups_dir),
MultiOptions::default())` after control's `migrate` and before serving
shard tenants (log `report.failed()`).

**For FB-174 (moves, add/drain/remove).** Add = provision the node, insert
the set and node rows, `activate_shard(router, &set, &db, ShardSetup {
backups_dir, replay: Some(&data_service), control_conninfo })`. Remove /
drain: `unsubscribe_from_control` before stopping the node. A move: the
target already has every collection's schema and policies (replay or
fan-out); recreate the tenant's direct logins with `copy_login_role`.
`_forgebase_index_builds` rows of a retired set go with the set (FK
cascade). Schema changes during a move run on both sets as on every other
(the freeze only concerns the tenant's rows).

**For FB-178 (topology tests in CI).** `forgebase_db::shard_testing`
(feature `testing`, Unix) starts clusters from `FORGEBASE_TEST_PG_ENGINE`
or `FORGEBASE_PG_ENGINE_DIR`; each test takes 15–60 s (initdb and
migrations per cluster); the suites above skip without an engine. Socket
paths must stay under 104 bytes: set `FORGEBASE_TEST_PG_TMP` to a short
directory.

## Addendum: FB-160, Apache AGE in the engine build (2026-11-11)

Built as §1.3 describes, with the three fixes of decision 9a.1 (option B)
carried as patches. Where the build decides something the text left open,
it is said here.

**Files.** `deploy/postgres-engine/age/{patches/0001..0004-*.patch,
make-args, post-install.sh, README.md}`;
`THIRD_PARTY_LICENSES/postgres/age/{age.txt, NOTICE}` (and one row in
`THIRD_PARTY_LICENSES/postgres/README.md`); `engine.lock` (`age` appended to
`contents.extensions`, `[sources.age]`, four `[[sources.age.patches]]`,
and the local `[archives."aarch64-apple-darwin"]` pin now names the r1
archive with AGE: AGE joins r1, no new revision);
`deploy/postgres-engine/smoke.sh` (the AGE checks FB-130 left to FB-160);
one step in `.github/workflows/postgres-engine.yml` (Bison 3 on the macOS
runners). Test-only Rust: `crates/forgebase-pgengine/tests/extensions.rs`
asserted that no engine ships AGE ("before FB-160"); both tests now branch on
`EngineDir::has_plugin("age")` and check the AGE side (owner
`forgebase_super`, both roles preload it, `graph = "on"` accepted). No
production Rust changed. `build-unix.sh` and `engine.py` are unchanged: the
FB-130 hook does everything.

**Source.** `apache-age-1.8.0-src.tar.gz` from
`archive.apache.org/dist/age/PG18/1.8.0/` (keeps every release;
`downloads.apache.org` drops 1.8.0 once a newer PG18 release is out). Its
SHA-512 matched the ASF `.sha512` fetched from `downloads.apache.org` (a
different server); the build checks the SHA-256 in `engine.lock`. The `.asc`
signature was not checked (no gpg on the build machine). Nothing else is
fetched: the patches are in the repository.

**Patches.** Applied in name order by `build-unix.sh` (`patch -p1
--forward`). `age/post-install.sh` fails the build when `patches/` and
`[[sources.age.patches]]` differ (names, order or SHA-256), before anything
is packed. I reviewed each diff against the 1.8.0 source:

| Patch | Upstream | Fixes | Review |
|---|---|---|---|
| `0001-pr2475-detach-delete-rls-segfault.patch` (SHA-256 `f62d9ad5…`) | apache/age#2475, commit `2e4b4ad91359b6e4dedbe787b83176f565bf7727`, verbatim | `DETACH DELETE` of a vertex with an edge whose label has an RLS policy calling a SQL function crashed the backend: the connected-edge check runs at executor shutdown with no active snapshot | 25 lines in `cypher_delete.c`: pushes `es_snapshot` when none is active and pops it at the end; the function has no early return between push and pop, and an error unwinds the snapshot stack at abort. Plus the upstream regression test |
| `0002-pr2561-rls-and-select-acl-in-traversals.patch` (`8ae84d4a…`) | apache/age#2561, commit `3eeb20bd967875cb32158d495396916d85697a51`, verbatim | variable-length and shortest-path traversals fill the global graph cache with a raw heap scan that skipped RLS and `SELECT` privileges | `age_global_graph.c`: label tables with RLS active for the current role load through an SPI read-only cursor (the planner applies the policies), others keep the heap scan after an explicit table/column `SELECT` check; edges to invisible vertices are dropped; a cache loaded through RLS is rebuilt every statement, others are keyed on role and the new GUC and invalidated by ACL/policy/role catalog changes; error paths free the label lists. New GUC `age.enforce_rls_in_traversal` (default `on`, `PGC_SUSET`). Applies cleanly; compiles without new warnings |
| `0003-pr2578-utility-commands-without-age.patch` (`46da0aa2…`) | apache/age#2578, commit `6e073cf40917731b4cd5b2654e4a93da005a2295`, **backported** | with AGE loaded in a session, `TRUNCATE` in a database without the extension failed reading AGE's catalog (`schema "ag_catalog" does not exist`) | upstream guards three branches of `ag_ProcessUtility_hook` (TRUNCATE, VACUUM FULL, CLUSTER) with `is_age_extension_exists()`; 1.8.0 has only the TRUNCATE branch (VACUUM/CLUSTER came with master commit `0e30566226`, #2551, after 1.8.0), so the backport keeps that hunk and the upstream tests unchanged. The patch header says so |

Failing before, proven: the smoke checks run on the same engine with an
unpatched 1.8.0 `age.dylib` gave a variable-length `MATCH` count of 2 (the
RLS-hidden vertex was visible; 1 expected), `DETACH DELETE` killed the
backend (`terminated by signal 11`), and `TRUNCATE` in a database without AGE
failed with `schema "ag_catalog" does not exist`. With the patches: 1, ok, ok.
AGE's own regression suite (43 tests, run with the build's `pg_regress`
against the extracted engine): 41 pass, including `security` (0001),
`rls_vle` (0002) and `drop` (0003). `cypher_merge` failed on a sort-order
difference from the temp instance's locale and passes with `--no-locale` (as
upstream CI runs it). `age_load` fails on macOS only: the CSV loaders accept
a file when its resolved path starts with `/tmp/age/`, and macOS resolves
`/tmp` to `/private/tmp`. Upstream behaviour, not patched; ForgeBase does not
use the CSV loaders (§1.5 writes label tables with SQL).

**A fourth fix found in review, carried: #2581 (human decision 2026-11-11).**
apache/age#2581 (commit `06ce678d74210539705c77e2dc7c9fa861e2a331`, master
only, 9 lines in `ag_catalog.c`): with AGE loaded in a session,
`post_parse_analyze` runs for the `ROLLBACK` of a failed transaction; when
that backend's caches were reset meanwhile (shared invalidation queue
overflow), `is_age_extension_exists()` reads `pg_extension` in the aborted
transaction, the locks leak into the PGPROC, and every later backend on it
fails with "failed to re-find shared lock object" until restart. 1.8.0 has
the same code and ForgeBase's per-role preload reaches it. FB-160b carries it
as `patches/0004-pr2581-no-catalog-read-outside-transaction.patch`
(SHA-256 `de29e981…`, upstream commit verbatim; re-reviewed: the code diff is
byte-identical to the PR head `4556939f` and to the merged commit, it returns
false outside a live transaction and caches nothing), with its
`[[sources.age.patches]]` table in `engine.lock`. #2581 carried, human decision 2026-11-11.
Failing before, proven: the new smoke check (session stopped while the
invalidation queue overflows, then `ROLLBACK`) on the unpatched 1.8.0 engine
fails with `ERROR: ResourceOwnerEnlarge called after release started`; with
0004 it passes and 99 concurrent connections take every free PGPROC without a
`failed to re-find shared lock object`. The aarch64-apple-darwin r1 archive
was rebuilt and re-pinned (SHA-256 `907d08d9…`, 7,857,923 bytes, +73 bytes).

**Build.** PGXS through the FB-130 hook. AGE's grammar is a GLR grammar
built with `-Werror -Wno-error=conflicts-sr`, which needs GNU Bison 3; macOS
ships 2.3. `age/make-args` sets `BISON` to the first Bison 3 among Homebrew's
two prefixes and `bison` on `PATH` (the build stops with a message when
there is none) and `YFLAGS=-Wno-deprecated` (Bison 3 reports AGE's
`%name-prefix` and `%pure-parser` as deprecated, an error under `-Werror`;
PostgreSQL's configure adds `-Wno-deprecated` only when it found Bison 3
itself). Flex 2.6.4 and Perl are the ones PostgreSQL's build uses. Local
build: Homebrew `bison` 3.8.2 installed (keg-only, nothing linked into
`PATH`). CI: the new step installs it on the macOS runners; Alpine's image
already has bison 3.8.

**`lib/plugins/age.*` is a link** (`../age.<so|dylib>`), not a copy:
PostgreSQL's loader recognises an already loaded library by inode, so the
extension's `$libdir/age` and the roles' `$libdir/plugins/age` are one
loaded module. Two copies would be loaded twice and install AGE's hooks
twice in one backend. The archive format already carries links;
`forgebase-pgengine`'s `has_plugin` follows them.

**Licences.** `share/licenses/age.txt` (upstream `LICENSE`: Apache-2.0 with
the PostgreSQL-licensed parts appended) and `age-NOTICE.txt` (upstream
`NOTICE`). `post-install.sh` compares both with the tarball's files byte for
byte. Windows ships neither (no AGE).

**Windows.** Unchanged: `build-windows.ps1` never builds AGE and writes
`graph_cypher = false`; `[sources.age] os = ["linux", "macos"]`, so Windows
neither fetches it nor needs its licence. `smoke.sh` now fails when the
manifest's `graph_cypher` and the presence of `lib/plugins/age.*` disagree.

**Size (aarch64-apple-darwin, local build).** Archive 7,857,850 bytes
(FB-130: 7,661,698; +196,152, +0.19 MiB). Unpacked 30,912,449 bytes in 945
files and 1 link (FB-130: 30,097,121 in 938); AGE's 7 files are 815,082
bytes (`age.dylib` 606,128 stripped, SQL 196,063, licences 12,891), the other
246 bytes are the licence README row: the rest of the engine is unchanged.
§1.3's estimate of 3–4 MB was high. Idle RSS of the smoke primary: 84,464
KiB in 10 processes, postmaster 25,408 KiB (FB-130: about 73 MiB, postmaster
25 MiB); AGE is not loaded by the postmaster, so the difference is run to run
noise.

**Smoke test** (`smoke.sh`, from the archive, passed): `CREATE EXTENSION
age`, `create_graph`; a role `fb_graph` (not a superuser) with
`session_preload_libraries = '$libdir/plugins/age'`, as `forgebase_server`
has; as that role `MATCH (a:Person)-[:KNOWS]->(b:Person)` and a
variable-length `MATCH` under RLS (0002); `DETACH DELETE` under a policy
calling a SQL function (0001); `TRUNCATE` in a database without AGE (0003);
`pg_dump` with a graph; and, on the hot standby started from
`pg_basebackup`, the same `MATCH` and variable-length `MATCH` as `fb_graph`.
§1.2 said plain `MATCH` on a standby is not covered upstream; it works here.
FB-163 keeps its formal standby test.

**musl.** Not decided here: there is no Linux build on this machine (no
Docker daemon running), so neither Linux target has been built with AGE. The
evidence so far: AGE's sources use no glibc-only interface (no
`execinfo.h`, `qsort_r`, `mallinfo`, `backtrace`; the one `uint` is defined
by musl under `_GNU_SOURCE`, which PostgreSQL sets on Linux); the Alpine
image has bison 3.8, flex and perl; `age.so` needs only libc, which the
build's NEEDED check allows. The engine workflow decides: if AGE fails to
build or `smoke.sh` fails on a musl target, that target ships without AGE as
Windows does. That fallback needs a small `build-unix.sh` change (not made:
skip an extension whose `[sources.<name>] os` lacks the target's OS, then set
`[sources.age] os = ["macos"]`); the manifest then says
`graph_cypher = false` and the server reports graph as portable.

**CI must prove** (nothing below ran here): the AGE build and `smoke.sh` on
x86_64 and aarch64 musl (Alpine 3.20) and on `x86_64-apple-darwin`
(`macos-15-intel`, Bison from `/usr/local/opt/bison`); the distro matrix with
AGE (`distros.sh` runs the same `smoke.sh`, so every distro runs the Cypher
and standby checks); that `shellcheck` accepts `smoke.sh` (it does here).

**Notes for FB-163 (openCypher service).**
- AGE loads in every `forgebase_server`/`forgebase_reader` session
  (`current_setting('age.enforce_rls_in_traversal')` answers `on` for both
  roles in the pgengine test). `SET LOCAL search_path = ag_catalog, …` per
  transaction as planned.
- `age.enforce_rls_in_traversal` is on by default and superuser-only to
  change: label rules compiled to RLS apply inside variable-length and
  shortest-path traversals. Cost: a graph whose labels use RLS rebuilds the
  traversal cache through SPI on every statement; measure it.
- With the GUC on, traversals also check `SELECT` on every label table (or
  every column): grant the reader role `SELECT` on the derived graph's
  tables, or traversals fail with `permission denied`.
- RLS policies can read agtype properties with `properties->>'"key"'` (the
  smoke test's `OWNS` policy does).
- `DETACH DELETE` under function policies is fixed, but §1.5 writes label
  tables with SQL anyway.
- `MATCH` and variable-length `MATCH` on a hot standby work as a
  preloading non-superuser (smoke test).
- Do not use AGE's CSV loaders (`load_labels_from_file`): restricted to
  `/tmp/age/`, unusable on macOS.
- Carry #2581 before openCypher sees real traffic (above).

## Addendum: FB-169, replica reads in the services (2026-11-11)

The §2.5 table, as code. Every call site below passes `ReadIntent::replica("<what>")`
to `Backend::fetch_routed` (FB-167); everything else keeps `fetch_all`. Without
replicas (SQLite, an unrouted PostgreSQL, a set with no healthy standby) a
routed read is the plain read, so nothing changes. A request needs nothing
more: the request scope (`X-ForgeBase-Read-After`, the principal's recent
commits, `X-ForgeBase-Consistency: strong`) already decides whether a replica
is eligible.

**Files.** `forgebase-db`: `routing/via.rs` (new), `routing/testing.rs`
(`RoutedCluster`), `backend.rs` and `router.rs` (`sample_commit_token`).
`forgebase-data` `lib.rs`, `list.rs`, `records.rs`, `sql.rs`;
`forgebase-vector` `service.rs`; `forgebase-graph` `service.rs`;
`forgebase-storage` `service.rs`, `objects.rs`, `http.rs`; `forgebase-queue`
`ops.rs`, `service.rs`, `pgmq/mod.rs`; `forgebase-jobs` `service.rs`;
`forgebase-realtime` `outbox.rs`, `conn.rs`; the JavaScript and Rust SDK
realtime clients. One `tests/replica_reads.rs` per crate. No migration.

**Helper.** `forgebase_db::ReadVia` is a `Backend` view whose `fetch_all` goes
through `fetch_routed`; everything else (transactions, `fetch_all_as`,
`explain`, `maintenance`) is the inner backend's. The vector, graph and jobs
services hand one to the helpers that run a call's reads, so a call site is one
line, not a signature change. `DataService::fetch_replica(stmt, what)` and
`QueueService::read_replica` / `StorageService::read_replica` do the same for
single statements.

**Call sites (reads a replica may serve).**

| Area | Call sites (`what`) |
|---|---|
| Records | `records.list` (list, search, count), `records.view`, `records.aggregate`, `records.expand`, `records.versions` (the versions and its visibility probes) |
| SQL editor | `sql.editor`: a read statement when the request body has `read_from_replica: true` (not with `show_secrets`) |
| Vector | `vector.get`, `vector.list` / `vector.get_collection` (item counts), `vector.stats` (counts, change sequence), `vector.search` and `vector.hybrid` on pgvector; hybrid keyword-only on every engine |
| Graph | `graph.get` (node, edge), `graph.list`, `graph.neighbors`, `graph.traverse`, `graph.shortest_path`, `graph.stats` |
| Storage | `storage.head`, `storage.list_objects`, `storage.list_versions`, `storage.list_buckets`, `storage.bucket_usage`, `storage.stats` |
| Queue | `queue.messages` (browser), `queue.dead_letters`, `queue.topic_metrics`, `queue.subscription_metrics`, `queue.backlog`, `queue.metrics` (rate buckets), the pgmq metrics and backlog reads |
| Jobs | `jobs.get`, `jobs.list`, `jobs.events`, `jobs.attempts`, `jobs.stats` |

"Dashboard statistics" are the aggregate, count and stats calls above; the
dashboard adds no query of its own.

**Primary only, kept (tested).**
- Every write and everything inside a write transaction; the catalogue
  (collection and index definitions, `fulltext_index` and `trigram_indexes`,
  reloads after DDL), policy snapshots.
- Reads as the RLS role (`fetch_all_as`): the role switch needs the primary's
  session. A native-RLS list stays on the primary.
- Graph rule names and access checks (`names`, `access`): a stale replica must
  never decide what a caller may see. Storage bucket rows and ACLs
  (`stored_bucket`, `load_acl`, `get_acl`), object *content* reads
  (`get_object`, signed URLs), uploads, multipart state.
- Queue claims, acks, nacks, deadline changes, releases, seeks, snapshots
  (pull path), publish, maintenance; pgmq reads fall back to the primary
  when a replica has not replayed a queue table yet (a statement error).
- Job claims, heartbeats, progress, checkpoints, completion, results,
  schedules (pg_cron, FB-165: primary only), workers.
- Vector writes, DDL, index builds, the progress poll, convert, reconcile; and
  kNN search and hybrid-with-a-vector on the portable engine: its in-process
  index follows the primary, so mixing it with replica store reads could drop
  hits. pgvector's HNSW is WAL-logged and replicates, so it routes.
- Realtime: the outbox tail, replay and the pruner (§2.5 "always primary").

**Realtime commit position (§2.6).** `Backend::sample_commit_token()` is a new
defaulted method: `None` everywhere except a `SetHandle` with replicas, which
answers `<set>/<current WAL insert LSN>`. `outbox::read_after` calls it once
per page, after the read (so the position is at or after every event of the
page) and stamps each event: `Event::commit`. A frame of a record event carries
it as `"commit"`; the JavaScript client and the Rust client feed it to their
commit tokens, so "event received, then read the record" sees the record. An
empty page samples nothing; a failed sample leaves the page unstamped.

**Tests.** `forgebase-db` `routing::testing::RoutedCluster` (feature
`testing`): a migrated primary, one standby, a control `SetHandle` routing over
it, `lag()` (pause replay), `sync()`, `catch_up()`, `replica_reads()` and
`fallbacks()`. Each crate's `tests/replica_reads.rs` pauses replay, writes on the
primary and checks, per call site, that the read saw the standby's older state
and that the standby's read counter moved; that a strong request and a
request's own writes see the primary; and that every primary-only path
(writes, claims, acks, catalogue, authorisation reads) leaves the counter
unchanged and fails nothing over (`fallbacks() == 0`). They need a PostgreSQL
engine (`FORGEBASE_PG_ENGINE_DIR`) and skip without one; SQLite has no replicas
and its behaviour is the unchanged suites.

**Breaking.** `Backend` has a new defaulted method (`sample_commit_token`).
`forgebase_realtime::Event` has a new public field `commit`. `SqlOptions` has a
new field `read_from_replica` (struct literals need `..Default::default()`).
Record-event frames may carry `commit` (additive).

**New API flag.** `POST /sql` and `/sql/batch` bodies accept
`read_from_replica` (boolean, default false); only a single read statement
without `show_secrets` uses it. OpenAPI fixtures need regenerating.

**Dashboard follow-ups.** The SQL editor gets a "Read from replica" toggle
(shown when `/api/system/database` reports healthy replicas) that sends
`read_from_replica: true`; results are as of the replica's replay position, so
show the lag note. Nothing else changes for the dashboard: its record lists,
statistics, queue and job screens are routed by the server.

**Notes for next packages.**
- FB-171 (server): build `RouterOptions { replicas, .. }` and a connector that
  implements `connect_replica`; the services take the control `SetHandle` as their
  `Backend` as before. Each `DataService` is per set, so the services already
  route within their own set. `attach_pgcron` stays on the primary handle.
  The realtime dispatcher per set stamps pages with that set's token.
- FB-172 (tenant moves): after a move the target's replicas are behind until
  they replay the copy: the move's last write returns a token; a read after it
  routes correctly through the same scope. Queue metrics may briefly read old
  counts (rates and backlog are not exact by design).
- FB-163 (Cypher on AGE): `graph.*` call sites already route; Cypher `MATCH`
  reads on replicas need the AGE graph on the standby (physical replication
  carries it); the stale marking after a move reads the primary.

## Addendum: FB-171, shard routing in the data core, realtime, functions and the server (2026-11-11)

Built as §3.5 and §3.6 describe, on FB-170's router, FB-169's replica
reads and FB-173's fan-out. Without shards (SQLite, one-set PostgreSQL)
every path below runs on the service's backend exactly as before; the
"sharded" checks are `DataService::is_sharded()` (the attached router has
more than one routable set). Human decision 2 (option A) is what is built.

**Files.** New: `crates/forgebase-data/src/shards.rs`,
`crates/forgebase-server/src/shards.rs`, and the tests
`crates/forgebase-data/tests/shard_routing.rs`,
`crates/forgebase-realtime/tests/shards.rs`,
`crates/forgebase-functions/tests/shards.rs`,
`crates/forgebase-server/tests/shards_fb171.rs`. Changed: data `lib.rs`,
`error.rs`, `guard.rs`, `records.rs`, `list.rs`, `changes.rs`,
`catalog.rs`, `ddl.rs`; realtime `lib.rs`, `outbox.rs`, `conn.rs`;
functions `lib.rs`; server `lib.rs`, `Cargo.toml` (dev-dependency
`forgebase-db` with `testing`, for the shard test clusters). No migration.

**Data core: the set per call.** `DataService::route(ctx, coll)`: a
tenant-scoped collection (and a view with `reads_tenant_data`) →
`router.for_tenant(ctx.tenant)` (control's handle is the service's own
backend); every other collection → control. Records create / get / update
/ delete, list, count, aggregate, versions and `expand` begin their
transaction or read on that backend (FB-169's `fetch_routed` per set:
`fetch_replica_on`). Collection DDL, indexes, the SQL editor and the
catalog stay on control (FB-173 fans DDL out). Router errors surface as
`DataError::Shard` with the router's code (`tenant_moving` 409,
`shard_migrating` / `shard_unavailable` 503).

**`tenant_required_sharded` (400).** On a sharded cluster a record call
on tenant data without a tenant (or with `""`) is refused in `resolve`,
privileged callers included (on one set they still see every tenant's
rows). Realtime subscriptions are the exception (`resolve_spanning`):
they fan in, below. The system context without a tenant is refused by
`route`.

**Relations across placements (tenant-scoped → shared).**
- **No foreign key.** `CatalogSnapshot` records whether
  `_forgebase_db_sets` has a non-retired shard row (one `count(*)` per
  catalog load; rows are control-only, so a shard reads 0).
  `ddl::relation_table` then returns no target for such a relation, so
  create / update collection, the shard steps of FB-173 and the catalog
  replay of a new shard create the column without `REFERENCES`.
  `DataService::drop_cross_placement_keys()` drops the keys that tables
  made before the first shard still have on control (PostgreSQL; a no-op
  while there is no shard row; idempotent); the server runs it at start on
  a sharded topology. Deviation from "no SQL foreign key" taken literally
  for control: a key kept on control would be harmless, but one rule for
  every set keeps `ON DELETE` in one place.
- **Existence check.** `check_relations` reads a shared target on
  control's primary (outside the writer's transaction); every other target
  in the writer's transaction as before.
- **`expand`.** Each hop reads its target's set: a shared target of tenant
  data is a second query on control (two steps, as §3.6 says).
- **`ON DELETE`.** Deleting a shared record whose collection is the target
  of tenant-scoped relations: `RESTRICT` (a required, non-cascading
  relation) is checked on every set's primary before control's delete
  (`409 record_referenced`, naming the set); the in-transaction walk skips
  tenant-scoped referrers; after the commit the deleted ids are handed to
  the installed `RelationFanout` (the server: job
  `data.relation_deletes`, reserved, 10 attempts), whose handler calls
  `DataService::apply_relation_deletes`: on every set, one transaction
  each, cascade (through the normal delete path: change events,
  tombstones, nested relations), set null, or removal of the id from
  multi-value relations, without the referencing collections' rules (as
  foreign-key actions). Idempotent. Without a hook the data core runs it
  right after the commit. Gap: a crash between control's commit and the
  enqueue loses the fan-out of that delete (no migration was added for an
  outbox of fan-outs; references then point to a missing record until the
  record is written again, and `expand` leaves it out).
- **`relation_crosses_shards` (400).** A rule, filter, sort key, group
  key or aggregate path that follows a relation from tenant data to a
  shared collection (at any hop) is refused on a sharded cluster: rules in
  `resolve` and in `expand`'s target rule, filters and paths in
  `build_list` / `aggregate`, realtime filters in `subscription_target`,
  and saved rules in `validate_predicate` (the policy service calls it in
  `put_policy`). The relation's own column (`author = 'id'`) is not a
  crossing. Realtime's per-event `view_snapshot` reads on the row's set.
- **The pre-check (open question 2).** `DataService::cross_shard_offenders()`
  parses every stored collection rule (`@request…` references replaced by a
  parameter) and lists `{collection, rule, path}` for each crossing; works
  on one set. The server logs them at start when sharded.

**Realtime.** Every set's outbox is tailed by its own dispatcher
(`SetTail`: set id, number, backend), started by a supervisor that follows
`DbRouter::subscribe` (sets added or removed later get or lose theirs);
commits wake every dispatcher (`Wakers`); the pruner prunes each set's
outbox. `RealtimeService::heads()` gives the newest cursor per set number;
`stats().head` and the welcome cursor are the maximum. A record
subscription has a **home**: one set (`Set(k)`: a tenant's data → its
tenant's set; shared collections and channels → control) or every set
(`All`: a privileged caller without a tenant on tenant data, sharded).
- One set: replay and catch-up read only that set's outbox (subscriptions
  are grouped by set); a cursor whose set (`cursor >> 40`) is not the
  home gets `reset` with reason **`tenant_moved`** (cursor: the home's
  head) instead of a silent gap. A live event of the subscriber's own
  tenant arriving from another set (the tenant moved while connected)
  re-homes the subscription and sends the same `reset` (cursor: the event's
  cursor − 1). Cursor `0` is "the beginning" of any set.
- Every set: live events only, last cursor kept per set; a subscription
  that brings a cursor, and a bus overflow (catch-up), get `reset` with
  reason **`cursor_unsupported_sharded`** and `cursor: null`; the
  `subscribed` reply's cursor is `null`.

**Functions.** The after-hook cursor (`after_hooks`) is stored on each set
(the table is `per_set`), next to its outbox: `FunctionService::open`
starts every current set's missing cursor at its head; a set opened later
has none and is read from its start; `run_after_hooks` passes over every
set in turn under the one lock (commit order within a set). A shard that
cannot be read is skipped and retried; control's error is returned. The
hook acts in the event's tenant, so its own data calls route to the
tenant's set.

**Server.** After control's `migrate`: `shards::open_router` (SQLite:
`DbRouter::single`; PostgreSQL: `DbRouter::open` with
`<data_dir>/topology.json`, default placement policy, freeze timeout and
replica options until FB-175's `[database.sharding]` / `[database.replicas]`
exist, and `NodeConnector`), then on a sharded topology
`shards::migrate_sets(&router, Some(backups), MultiOptions::default())`
(failures logged; such sets stay `migrating`). Policy, auth and the data
service get **control's `SetHandle`** as their backend (replica routing per
§2.5 now applies in the server). In `open_auth`:
`data.attach_shards(router)` before `enable_native_rls`. Then
`twophase::spawn_recovery(router, RECOVERY_INTERVAL)` on PostgreSQL (kept
in `RunningServer`, dropped in shutdown), `prepare_data` (drop the
cross-placement keys, log rule offenders), the relation fan-out hook and
its job handler. Shutdown closes every shard's pools.
`RunningServer::router()` is new. Per-set loops: realtime and after-hooks
here; jobs, queues, KV, vector and storage are FB-172's.
- **`NodeConnector`** (`SetConnector`): unseals `url_sealed` (lowercase hex
  of `Keyring::seal(url, aad = "forgebase/db-node/<node id>")`, the data
  directory's master key) and opens the node with the server's pool
  settings (`read_only` for replicas). `seal_node_url` / `open_node_url`
  are public for FB-175 (node URL sealing). Local nodes (no URL) are
  refused until FB-174's shard provisioning opens them.

**SQL editor (§3.6, after FB-153).**
- **Node selector:** `SqlOptions.set` (body field `set` of `POST /sql` and
  `/sql/batch`, the set **number**; absent or `0`: control). A read runs on
  that set's primary, or with `read_from_replica` on one of its replicas;
  statements that write (and every batch) run on control only (`400
  sql_set_read_only`: tenant data changes through the data API, schema
  through the collections API on every set; `forgebase db psql --node`
  reaches a shard). Unknown set: `404 shard_not_found`; a migrating set:
  `503 shard_migrating`. The number (not the id) keeps `SqlOptions` `Copy`.
- **"Read from replica" under the editor's role:** new defaulted
  `Backend::fetch_routed_as(stmt, intent, run_as)`. `SetHandle` routes it
  through `ReplicaSet::fetch_as`: the chosen replica's read-only
  transaction runs `SET LOCAL ROLE` and the claims exactly as
  `fetch_all_as` on the primary (a physical standby has the roles; both are
  allowed in read-only transactions); strong requests, lagging replicas and
  fallbacks read the primary as the role. `ReadVia` forwards it; everything
  else is `fetch_all_as`. `sql.rs` uses it when `read_from_replica` is set
  and `show_secrets` is not, so FB-153's `forgebase_sql_admin` boundary and
  FB-169's replica reads both hold. `SetHandle` now also forwards FB-153's
  `admin_batch` (the services run on control's handle).

**Not done here.** No dashboard change (the SQL editor's set selector and
the replica toggle are dashboard follow-ups). Extensions on shards
(`setup_extensions` runs on control only; FB-173 noted shards need the same
extensions).

**Tests.** `shard_routing.rs` (data): one set unchanged and the pre-check
(both backends); sharded (real clusters): placement of records, existence
on control, two-step expand, `tenant_required_sharded`, every
`relation_crosses_shards` path, no foreign keys on the replayed shard and
dropped on control, `RESTRICT` across sets, cascade / set null /
multi-value removal on both sets, idempotent fan-out. Realtime
`shards.rs`: per-set dispatch and cursor ranges, shared collections from
control, replay from the home set, `tenant_moved`, fan-in with
`cursor_unsupported_sharded`, per-set pruning. Functions `shards.rs`:
per-set cursors and hooks for events on both sets, once. Data
`routed_as.rs` (`RoutedCluster`): `fetch_routed_as` on the standby and the
primary as a role, and the SQL editor reading the replica as
`forgebase_sql_admin`; the node selector in `shard_routing.rs`. Server
`shards_fb171.rs`: one set (SQLite); sharded: the router opens the shard
through its sealed URL, `topology.json`, a dispatcher and an after-hook
cursor per set, records on the shard, the relation fan-out job.

**Breaking.** `DataError::Shard` (new variant). `SqlOptions` has a new
field `set` (struct literals need `..Default::default()`). `Backend` has a
new defaulted method `fetch_routed_as`. Record calls on tenant data
without a tenant answer `400 tenant_required_sharded` on sharded clusters.
Rules / filters / sort keys across placements answer `400
relation_crosses_shards` on sharded clusters (and such rules cannot be
saved). Realtime: `subscribed.cursor` and `reset.cursor` may be `null`
(spanning subscriptions); reset reasons `tenant_moved` and
`cursor_unsupported_sharded`. `DataService::delete_in_tx` gained a
parameter (crate-internal). Server: policy, auth and data run on control's
`SetHandle` instead of the `Database`.

**For FB-172.** Constructor arguments in `start_on` take `router.clone()`
(it is built right after `migrate`, before `open_auth`); one loop per
`router.sets()` entry, watch `router.subscribe()` for new sets (see the
realtime supervisor); control's handle for control-only work is
`router.control()`.
**For FB-174.** Before activating the first shard (its set row inserted):
`data.cross_shard_offenders()` (refuse or warn) and
`data.drop_cross_placement_keys()`. A move: realtime re-homes live
subscriptions on the first event from the new set and answers old cursors
with `tenant_moved`; after-hook cursors are per set, so events copied by a
move are not re-run (the copy writes no outbox rows through the data core).
**For FB-175.** Store node URLs with `forgebase_server::shards::seal_node_url`
(or the same format). Add `[database.sharding]` / `[database.replicas]` to
`RouterOptions` in `shards::open_router`.
