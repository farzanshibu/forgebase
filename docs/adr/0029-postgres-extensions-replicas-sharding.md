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
